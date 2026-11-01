//! Drawing the cell grid and the window's frame: the pipelines, the glyph atlas texture,
//! the table of its entries, and for each frame in flight a copy of the cells, the
//! frame's quads and the pixels to upload.

#![expect(unsafe_code, reason = "Vulkan is only reached through ash's raw calls")]

use std::collections::HashMap;
use std::io::Cursor;
use std::ops::Range;

use ash::vk;

use crate::atlas::{self, GpuGlyph, Upload};
use crate::chrome::Quad;
use crate::gpu::{During, GpuError};
use crate::grid::GpuCell;

/// What the shaders are given with each draw, laid out as `common.glsl` declares it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Constants {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub cell_width: i32,
    pub cell_height: i32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub columns: u32,
    pub pass: u32,
    pub cursor_column: i32,
    pub cursor_line: i32,
    pub cursor_shape: u32,
    pub cursor_color: u32,
    pub background: u32,
    pub selection: u32,
    pub baseline: i32,
    pub underline_top: i32,
    pub underline_thickness: i32,
    pub strikeout_top: i32,
    pub strikeout_thickness: i32,
    pub first: u32,
}

/// The cursor covers two cells.
pub const CURSOR_WIDE: u32 = 16;
/// The window has the focus, so a block cursor is filled.
pub const CURSOR_FOCUSED: u32 = 32;

/// A pane's rows that changed since its last job, and how it is drawn; the renderer sets
/// where its cells start.
pub struct PaneJob {
    pub terminal: u32,
    pub columns: usize,
    pub lines: usize,
    /// Each changed row's line and cells.
    pub rows: Vec<(usize, Vec<GpuCell>)>,
    pub constants: Constants,
}

/// What a frame shows, as the window hands it to be drawn.
pub struct Job {
    pub panes: Vec<PaneJob>,
    /// The atlas the glyphs are of.
    pub generation: u64,
    /// The atlas entries made since the last job, or all of them in a new atlas.
    pub entries: Vec<GpuGlyph>,
    pub uploads: Vec<Upload>,
    /// What the frame's quads are drawn with.
    pub constants: Constants,
    /// The window's frame: the first `under` quads drawn under the cells, the rest over
    /// them.
    pub quads: Vec<Quad>,
    pub under: usize,
    /// What the frame is cleared to, premultiplied.
    pub background: [f32; 4],
}

/// A pane's cells as the renderer keeps them, with the rows each frame's copy lacks.
struct Mirror {
    columns: usize,
    lines: usize,
    cells: Vec<GpuCell>,
    stale: Vec<Vec<bool>>,
    /// The rows changed since a frame last went to the surface.
    changed: Vec<bool>,
    /// What it was drawn with then; none for a size it has not been shown at.
    presented: Option<Constants>,
}

/// What the last frame on the surface showed besides the panes' cells.
struct Presented {
    extent: vk::Extent2D,
    generation: u64,
    /// What the frame was cleared to, each part as its bits.
    background: [u32; 4],
    quads: Vec<Quad>,
    under: usize,
    panes: Vec<u32>,
}

impl Constants {
    /// The same with what depends only on the cursor left out, and on where the pane's
    /// cells are in the buffer, which does not show.
    fn without_cursor(self) -> Self {
        Self {
            pass: 0,
            first: 0,
            cursor_column: 0,
            cursor_line: 0,
            cursor_shape: 0,
            cursor_color: 0,
            ..self
        }
    }

    fn cursor(self) -> (i32, i32, u32, u32) {
        (
            self.cursor_column,
            self.cursor_line,
            self.cursor_shape,
            self.cursor_color,
        )
    }
}

/// How far glyphs reach above and below the top of their cell, in device pixels. They are
/// not cut off at their cell, so what changes in a row can show in the rows around it.
#[derive(Debug, Clone, Copy, Default)]
struct Reach {
    above: i32,
    below: i32,
}

impl Reach {
    fn take_in(&mut self, glyph: &GpuGlyph) {
        let top = i32::from((glyph.offset >> 16) as i16);
        let height = (glyph.size >> 16) as i32;
        self.above = self.above.max(-top);
        self.below = self.below.max(top + height);
    }
}

/// The parts of an `extent` image that show the `lines` marked in a pane drawn with
/// `constants`, the whole width of the image for each: glyphs reach past their cells.
fn changed_rects(
    lines: impl Iterator<Item = bool>,
    constants: &Constants,
    reach: Reach,
    extent: vk::Extent2D,
) -> Vec<vk::RectLayerKHR> {
    let mut rects: Vec<vk::RectLayerKHR> = Vec::new();
    let mut add = |line: i32| {
        let row = constants.origin_y + line * constants.cell_height;
        let top = (row - reach.above).clamp(0, extent.height as i32);
        let below = reach.below.max(constants.cell_height);
        let bottom = (row + below).clamp(0, extent.height as i32);
        if bottom <= top {
            return;
        }
        if let Some(last) = rects.last_mut()
            && last.offset.y + last.extent.height as i32 >= top
        {
            last.extent.height = (bottom - last.offset.y) as u32;
            return;
        }
        rects.push(vk::RectLayerKHR {
            offset: vk::Offset2D { x: 0, y: top },
            extent: vk::Extent2D {
                width: extent.width,
                height: (bottom - top) as u32,
            },
            layer: 0,
        });
    };
    for (line, changed) in lines.enumerate() {
        if changed {
            add(line as i32);
        }
    }
    rects
}

struct Buffer {
    handle: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: u64,
}

/// Glyph entries the table holds before it is made again, larger.
const GLYPHS: u64 = 4096;

/// Quads a frame's buffer holds before it is made again, larger.
const QUADS: u64 = 64;

pub struct Renderer {
    memory: vk::PhysicalDeviceMemoryProperties,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    frame_pipeline: vk::Pipeline,
    pool: vk::DescriptorPool,
    sets: Vec<vk::DescriptorSet>,
    atlas: vk::Image,
    atlas_memory: vk::DeviceMemory,
    atlas_view: vk::ImageView,
    sampler: vk::Sampler,
    /// The atlas has its layout: it starts undefined.
    atlas_ready: bool,
    glyphs: Buffer,
    glyphs_written: usize,
    /// The atlas the texture and the table hold glyphs of.
    generation: u64,
    /// Every entry of that atlas, to write into a larger table.
    entries: Vec<GpuGlyph>,
    /// The cells of each pane drawn last, by terminal.
    mirrors: HashMap<u32, Mirror>,
    /// None when the surface shows nothing to go on from: the next frame changes it all.
    presented: Option<Presented>,
    reach: Reach,
    /// Glyphs to copy into the atlas texture with the next frame.
    uploads: Vec<Upload>,
    cells: Vec<Option<Buffer>>,
    /// For each frame's cells, the panes they hold: terminal, first cell, columns and
    /// lines.
    placed: Vec<Vec<(u32, usize, usize, usize)>>,
    quads: Vec<Buffer>,
    staging: Vec<Option<Buffer>>,
}

impl Renderer {
    pub fn new(
        instance: &ash::Instance,
        physical: vk::PhysicalDevice,
        device: &ash::Device,
        format: vk::Format,
        frames: usize,
    ) -> Result<Self, GpuError> {
        // SAFETY: the physical device came from this instance.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let set_layout = make_set_layout(device)?;
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(size_of::<Constants>() as u32)];
        let layouts = [set_layout];
        // SAFETY: as above.
        let layout = unsafe {
            device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&layouts)
                    .push_constant_ranges(&ranges),
                None,
            )
        }
        .during("vkCreatePipelineLayout")?;
        let pipeline = make_pipeline(
            device,
            layout,
            format,
            include_bytes!(concat!(env!("OUT_DIR"), "/cells.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/cells.frag.spv")),
        )?;
        let frame_pipeline = make_pipeline(
            device,
            layout,
            format,
            include_bytes!(concat!(env!("OUT_DIR"), "/frame.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/frame.frag.spv")),
        )?;

        let (pool, sets) = make_sets(device, set_layout, frames)?;

        let (atlas, atlas_memory, atlas_view) = make_atlas(device, &memory)?;
        // Glyphs are drawn a texel to a pixel, so nothing is filtered.
        let info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .unnormalized_coordinates(false);
        // SAFETY: the device is live.
        let sampler = unsafe { device.create_sampler(&info, None) }.during("vkCreateSampler")?;
        let glyphs = make_buffer(
            device,
            &memory,
            GLYPHS * size_of::<GpuGlyph>() as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        )?;
        let quads = (0..frames)
            .map(|_| {
                make_buffer(
                    device,
                    &memory,
                    QUADS * size_of::<Quad>() as u64,
                    vk::BufferUsageFlags::STORAGE_BUFFER,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        let renderer = Self {
            memory,
            set_layout,
            layout,
            pipeline,
            frame_pipeline,
            pool,
            sets,
            atlas,
            atlas_memory,
            atlas_view,
            sampler,
            atlas_ready: false,
            glyphs,
            glyphs_written: 0,
            generation: 0,
            entries: Vec::new(),
            mirrors: HashMap::new(),
            presented: None,
            reach: Reach::default(),
            uploads: Vec::new(),
            cells: (0..frames).map(|_| None).collect(),
            placed: vec![Vec::new(); frames],
            quads,
            staging: (0..frames).map(|_| None).collect(),
        };
        for frame in 0..frames {
            renderer.write_atlas_and_glyphs(device, frame);
            renderer.write_quads(device, frame);
        }
        Ok(renderer)
    }

    /// Copies what changed into frame `frame`'s buffers and records the frame into
    /// `commands`, drawing to `view`. The caller has waited for the frame's last use of
    /// its buffers to end.
    pub fn record(
        &mut self,
        device: &ash::Device,
        commands: vk::CommandBuffer,
        frame: usize,
        view: vk::ImageView,
        extent: vk::Extent2D,
        job: &mut Job,
    ) -> Result<(), GpuError> {
        self.copy_cells(device, frame, &mut job.panes)?;
        self.copy_glyphs(device)?;
        self.copy_quads(device, frame, &job.quads)?;
        let mut uploads = std::mem::take(&mut self.uploads);
        self.upload(device, commands, frame, &mut uploads)?;

        let attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: job.background,
                },
            })];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        let rendering = vk::RenderingInfo::default()
            .render_area(area)
            .layer_count(1)
            .color_attachments(&attachments);
        let viewport = vk::Viewport::default()
            .width(extent.width as f32)
            .height(extent.height as f32)
            .max_depth(1.0);
        let stages = vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT;
        // SAFETY: the command buffer is recording, and every handle is this device's.
        unsafe {
            device.cmd_begin_rendering(commands, &rendering);
            device.cmd_set_viewport(commands, 0, &[viewport]);
            device.cmd_set_scissor(commands, 0, &[area]);
            device.cmd_bind_descriptor_sets(
                commands,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[self.sets[frame]],
                &[],
            );
            let draw_quads = |quads: Range<usize>| {
                if quads.is_empty() {
                    return;
                }
                device.cmd_bind_pipeline(
                    commands,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.frame_pipeline,
                );
                device.cmd_push_constants(
                    commands,
                    self.layout,
                    stages,
                    0,
                    bytes_of(&job.constants),
                );
                device.cmd_draw(commands, 6, quads.len() as u32, 0, quads.start as u32);
            };
            let under = job.under.min(job.quads.len());
            draw_quads(0..under);
            device.cmd_bind_pipeline(commands, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            for pane in &job.panes {
                let cells = (pane.columns * pane.lines) as u32;
                if cells == 0 {
                    continue;
                }
                for pass in 0..2 {
                    let constants = Constants {
                        pass,
                        ..pane.constants
                    };
                    device.cmd_push_constants(
                        commands,
                        self.layout,
                        stages,
                        0,
                        bytes_of(&constants),
                    );
                    device.cmd_draw(commands, 6, cells, 0, pane.constants.first);
                }
            }
            draw_quads(under..job.quads.len());
            device.cmd_end_rendering(commands);
        }
        Ok(())
    }

    /// Takes in what `job` brings, whether or not it is drawn: the glyphs and the panes'
    /// rows, which later frames build on.
    ///
    /// # Errors
    ///
    /// When the device is lost.
    pub fn take(&mut self, device: &ash::Device, job: &mut Job) -> Result<(), GpuError> {
        if job.generation != self.generation {
            // Frames in flight sample the old glyphs, which the new ones overwrite.
            // SAFETY: the device is live.
            unsafe { device.device_wait_idle() }.during("vkDeviceWaitIdle")?;
            self.generation = job.generation;
            self.glyphs_written = 0;
            self.entries.clear();
            self.uploads.clear();
        }
        for entry in &job.entries {
            self.reach.take_in(entry);
        }
        self.entries.append(&mut job.entries);
        self.uploads.append(&mut job.uploads);
        self.take_in(&mut job.panes);
        Ok(())
    }

    /// Takes the rows the panes sent into their mirrors, forgetting the panes not drawn
    /// now.
    fn take_in(&mut self, panes: &mut [PaneJob]) {
        let copies = self.cells.len();
        self.mirrors
            .retain(|terminal, _| panes.iter().any(|pane| pane.terminal == *terminal));
        for pane in panes {
            let mirror = self.mirrors.entry(pane.terminal).or_insert_with(|| Mirror {
                columns: 0,
                lines: 0,
                cells: Vec::new(),
                stale: vec![Vec::new(); copies],
                changed: Vec::new(),
                presented: None,
            });
            if (mirror.columns, mirror.lines) != (pane.columns, pane.lines) {
                mirror.columns = pane.columns;
                mirror.lines = pane.lines;
                mirror.cells = vec![GpuCell::default(); pane.columns * pane.lines];
                mirror.stale = vec![vec![true; pane.lines]; copies];
                mirror.changed = vec![true; pane.lines];
                mirror.presented = None;
            }
            for (line, row) in std::mem::take(&mut pane.rows) {
                let start = line * mirror.columns;
                let Some(cells) = mirror.cells.get_mut(start..start + mirror.columns) else {
                    continue;
                };
                if row.len() != cells.len() {
                    continue;
                }
                cells.copy_from_slice(&row);
                for stale in &mut mirror.stale {
                    stale[line] = true;
                }
                mirror.changed[line] = true;
            }
        }
    }

    /// What `job`, drawn in `extent` of an image, changes from the last frame on the
    /// surface, for the compositor to compose only that; none when it may be anything.
    /// Empty when the frame would look the same.
    pub fn changes(&self, job: &Job, extent: vk::Extent2D) -> Option<Vec<vk::RectLayerKHR>> {
        let presented = self.presented.as_ref()?;
        let same = presented.extent == extent
            && presented.generation == job.generation
            && presented.background == job.background.map(f32::to_bits)
            && presented.under == job.under
            && presented.quads == job.quads
            && presented
                .panes
                .iter()
                .copied()
                .eq(job.panes.iter().map(|pane| pane.terminal));
        if !same {
            return None;
        }
        let mut rects = Vec::new();
        for pane in &job.panes {
            let mirror = self.mirrors.get(&pane.terminal)?;
            let before = mirror.presented?;
            if before.without_cursor() != pane.constants.without_cursor() {
                return None;
            }
            let moved = before.cursor() != pane.constants.cursor();
            let lines = mirror.changed.iter().enumerate().map(|(line, &changed)| {
                let line = line as i32;
                changed
                    || (moved && (line == before.cursor_line || line == pane.constants.cursor_line))
            });
            rects.extend(changed_rects(lines, &pane.constants, self.reach, extent));
        }
        Some(rects)
    }

    /// `job` went to the surface in `extent` of an image.
    pub fn presented(&mut self, job: &mut Job, extent: vk::Extent2D) {
        for pane in &job.panes {
            if let Some(mirror) = self.mirrors.get_mut(&pane.terminal) {
                mirror.changed.fill(false);
                mirror.presented = Some(pane.constants);
            }
        }
        self.presented = Some(Presented {
            extent,
            generation: job.generation,
            background: job.background.map(f32::to_bits),
            quads: std::mem::take(&mut job.quads),
            under: job.under,
            panes: job.panes.iter().map(|pane| pane.terminal).collect(),
        });
    }

    /// The surface takes images of another swapchain from now on.
    pub fn forget_presented(&mut self) {
        self.presented = None;
    }

    /// Copies the rows frame `frame`'s cells lack, one pane after another; all of them
    /// into a buffer just made or once the panes moved in it.
    fn copy_cells(
        &mut self,
        device: &ash::Device,
        frame: usize,
        panes: &mut [PaneJob],
    ) -> Result<(), GpuError> {
        let mut placed = Vec::with_capacity(panes.len());
        let mut total = 0;
        for pane in panes.iter_mut() {
            pane.constants.first = total as u32;
            placed.push((pane.terminal, total, pane.columns, pane.lines));
            total += pane.columns * pane.lines;
        }
        let needed = (total.max(1) * size_of::<GpuCell>()) as u64;
        let fresh = self.cells[frame]
            .as_ref()
            .is_none_or(|buffer| buffer.size < needed);
        if fresh {
            if let Some(old) = self.cells[frame].take() {
                destroy_buffer(device, &old);
            }
            // Room to grow, so that widening the window a little does not make it again.
            let buffer = make_buffer(
                device,
                &self.memory,
                needed + needed / 2,
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )?;
            self.cells[frame] = Some(buffer);
            self.write_cells(device, frame);
        }
        let moved = fresh || self.placed[frame] != placed;
        let Some(buffer) = self.cells[frame].as_ref() else {
            return Ok(());
        };
        for &(terminal, first, columns, lines) in &placed {
            let Some(mirror) = self.mirrors.get_mut(&terminal) else {
                continue;
            };
            let stale = &mut mirror.stale[frame];
            for line in (0..lines).filter(|&line| moved || stale[line]) {
                let row = &mirror.cells[line * columns..(line + 1) * columns];
                // SAFETY: the buffer is mapped and holds every pane's cells; the GPU is
                // done with this frame's copy.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        row.as_ptr().cast::<u8>(),
                        buffer
                            .mapped
                            .add((first + line * columns) * size_of::<GpuCell>()),
                        size_of_val(row),
                    );
                }
            }
            stale.fill(false);
        }
        self.placed[frame] = placed;
        Ok(())
    }

    /// Copies the window's frame into frame `frame`'s quads, in a larger buffer if they
    /// outgrow it.
    fn copy_quads(
        &mut self,
        device: &ash::Device,
        frame: usize,
        quads: &[Quad],
    ) -> Result<(), GpuError> {
        let needed = size_of_val(quads) as u64;
        if needed > self.quads[frame].size {
            let larger = make_buffer(
                device,
                &self.memory,
                needed * 2,
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )?;
            destroy_buffer(device, &std::mem::replace(&mut self.quads[frame], larger));
            self.write_quads(device, frame);
        }
        // SAFETY: the buffer is mapped and large enough, and the GPU is done with this
        // frame's.
        unsafe {
            std::ptr::copy_nonoverlapping(
                quads.as_ptr().cast::<u8>(),
                self.quads[frame].mapped,
                size_of_val(quads),
            );
        }
        Ok(())
    }

    /// Appends the atlas entries made since the last frame to the table, which every
    /// frame shares: an entry is written before any cell refers to it.
    fn copy_glyphs(&mut self, device: &ash::Device) -> Result<(), GpuError> {
        let entries = &self.entries;
        let needed = size_of_val(entries) as u64;
        if needed > self.glyphs.size {
            // Frames in flight read the table, so it is replaced only once they are done.
            // SAFETY: the device is live.
            unsafe { device.device_wait_idle() }.during("vkDeviceWaitIdle")?;
            let larger = make_buffer(
                device,
                &self.memory,
                needed * 2,
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )?;
            destroy_buffer(device, &std::mem::replace(&mut self.glyphs, larger));
            self.glyphs_written = 0;
            for frame in 0..self.sets.len() {
                self.write_atlas_and_glyphs(device, frame);
            }
        }
        let new = &entries[self.glyphs_written..];
        // SAFETY: the buffer is mapped and large enough; the GPU reads only entries
        // already written, none of which is written again.
        unsafe {
            std::ptr::copy_nonoverlapping(
                new.as_ptr().cast::<u8>(),
                self.glyphs
                    .mapped
                    .add(self.glyphs_written * size_of::<GpuGlyph>()),
                size_of_val(new),
            );
        }
        self.glyphs_written = entries.len();
        Ok(())
    }

    /// Records copies of the glyphs rasterised since the last frame into the atlas.
    fn upload(
        &mut self,
        device: &ash::Device,
        commands: vk::CommandBuffer,
        frame: usize,
        uploads: &mut Vec<Upload>,
    ) -> Result<(), GpuError> {
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        if !self.atlas_ready {
            let barrier = [vk::ImageMemoryBarrier2::default()
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .image(self.atlas)
                .subresource_range(range)];
            // SAFETY: the command buffer is recording.
            unsafe {
                device.cmd_pipeline_barrier2(
                    commands,
                    &vk::DependencyInfo::default().image_memory_barriers(&barrier),
                );
            }
            self.atlas_ready = true;
        }
        if uploads.is_empty() {
            return Ok(());
        }
        let needed: u64 = uploads
            .iter()
            .map(|upload| upload.pixels.len() as u64)
            .sum();
        if self.staging[frame]
            .as_ref()
            .is_none_or(|buffer| buffer.size < needed)
        {
            if let Some(old) = self.staging[frame].take() {
                destroy_buffer(device, &old);
            }
            self.staging[frame] = Some(make_buffer(
                device,
                &self.memory,
                needed.max(1 << 20),
                vk::BufferUsageFlags::TRANSFER_SRC,
            )?);
        }
        let Some(staging) = self.staging[frame].as_ref() else {
            return Ok(());
        };
        let regions = stage(staging, uploads);
        // Earlier frames may still sample the atlas; the copies only touch texels none of
        // them uses, but the barriers keep the accesses ordered all the same.
        let before = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .src_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .image(self.atlas)
            .subresource_range(range)];
        let after = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .image(self.atlas)
            .subresource_range(range)];
        // SAFETY: the command buffer is recording; the regions lie in the staging
        // buffer and the atlas.
        unsafe {
            device.cmd_pipeline_barrier2(
                commands,
                &vk::DependencyInfo::default().image_memory_barriers(&before),
            );
            device.cmd_copy_buffer_to_image(
                commands,
                staging.handle,
                self.atlas,
                vk::ImageLayout::GENERAL,
                &regions,
            );
            device.cmd_pipeline_barrier2(
                commands,
                &vk::DependencyInfo::default().image_memory_barriers(&after),
            );
        }
        Ok(())
    }

    fn write_atlas_and_glyphs(&self, device: &ash::Device, frame: usize) {
        let images = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(self.atlas_view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let glyphs = [vk::DescriptorBufferInfo::default()
            .buffer(self.glyphs.handle)
            .range(vk::WHOLE_SIZE)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.sets[frame])
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&images),
            vk::WriteDescriptorSet::default()
                .dst_set(self.sets[frame])
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&glyphs),
        ];
        // SAFETY: the set is not in use: its frame is done, or none has been drawn.
        unsafe { device.update_descriptor_sets(&writes, &[]) };
    }

    fn write_cells(&self, device: &ash::Device, frame: usize) {
        let Some(cells) = self.cells[frame].as_ref() else {
            return;
        };
        let buffers = [vk::DescriptorBufferInfo::default()
            .buffer(cells.handle)
            .range(vk::WHOLE_SIZE)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(self.sets[frame])
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&buffers)];
        // SAFETY: as above.
        unsafe { device.update_descriptor_sets(&writes, &[]) };
    }

    fn write_quads(&self, device: &ash::Device, frame: usize) {
        let buffers = [vk::DescriptorBufferInfo::default()
            .buffer(self.quads[frame].handle)
            .range(vk::WHOLE_SIZE)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(self.sets[frame])
            .dst_binding(3)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&buffers)];
        // SAFETY: as above.
        unsafe { device.update_descriptor_sets(&writes, &[]) };
    }

    /// Destroys everything; the device is idle.
    pub fn destroy(&mut self, device: &ash::Device) {
        for buffer in self.cells.drain(..).chain(self.staging.drain(..)).flatten() {
            destroy_buffer(device, &buffer);
        }
        for buffer in self.quads.drain(..) {
            destroy_buffer(device, &buffer);
        }
        destroy_buffer(device, &self.glyphs);
        // SAFETY: the caller made the device idle; these are its objects, destroyed once.
        unsafe {
            device.destroy_sampler(self.sampler, None);
            device.destroy_image_view(self.atlas_view, None);
            device.destroy_image(self.atlas, None);
            device.free_memory(self.atlas_memory, None);
            device.destroy_descriptor_pool(self.pool, None);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline(self.frame_pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

fn bytes_of(constants: &Constants) -> &[u8] {
    // SAFETY: `Constants` is plain 32-bit fields with no padding.
    unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(constants).cast::<u8>(),
            size_of::<Constants>(),
        )
    }
}

fn make_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    format: vk::Format,
    vertex: &[u8],
    fragment: &[u8],
) -> Result<vk::Pipeline, GpuError> {
    let module = |spirv: &[u8]| -> Result<vk::ShaderModule, GpuError> {
        let code = ash::util::read_spv(&mut Cursor::new(spirv)).map_err(|_| GpuError::Shader)?;
        // SAFETY: the code is SPIR-V glslc made, read into aligned words.
        unsafe {
            device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)
        }
        .during("vkCreateShaderModule")
    };
    let vertex = module(vertex)?;
    let fragment = module(fragment)?;
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vertex)
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(fragment)
            .name(c"main"),
    ];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    // Premultiplied colour over what is below.
    let blend = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)];
    let blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
    let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
    let formats = [format];
    let mut rendering =
        vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&rasterization)
        .multisample_state(&multisample)
        .color_blend_state(&blending)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut rendering);
    // SAFETY: the infos and the modules live through the call.
    let made =
        unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) };
    // SAFETY: the pipeline keeps what it needs of the modules.
    unsafe {
        device.destroy_shader_module(vertex, None);
        device.destroy_shader_module(fragment, None);
    }
    let pipelines = made.map_err(|(_, result)| GpuError::Call {
        call: "vkCreateGraphicsPipelines",
        result,
    })?;
    Ok(pipelines[0])
}

fn memory_type(
    memory: &vk::PhysicalDeviceMemoryProperties,
    allowed: u32,
    wanted: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..memory.memory_type_count).find(|&index| {
        allowed & (1 << index) != 0
            && memory.memory_types[index as usize]
                .property_flags
                .contains(wanted)
    })
}

/// A buffer of `size` bytes the CPU writes through a mapping that stays.
fn make_buffer(
    device: &ash::Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<Buffer, GpuError> {
    let info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: the device is live.
    let handle = unsafe { device.create_buffer(&info, None) }.during("vkCreateBuffer")?;
    // SAFETY: the buffer was just made by this device.
    let requirements = unsafe { device.get_buffer_memory_requirements(handle) };
    let Some(kind) = memory_type(
        memory,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ) else {
        // SAFETY: nothing else has the buffer.
        unsafe { device.destroy_buffer(handle, None) };
        return Err(GpuError::NoDevice);
    };
    let info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(kind);
    // SAFETY: as above; the memory is bound and mapped once, here.
    unsafe {
        let memory = device
            .allocate_memory(&info, None)
            .during("vkAllocateMemory")?;
        device
            .bind_buffer_memory(handle, memory, 0)
            .during("vkBindBufferMemory")?;
        let mapped = device
            .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
            .during("vkMapMemory")?
            .cast::<u8>();
        Ok(Buffer {
            handle,
            memory,
            mapped,
            size,
        })
    }
}

fn destroy_buffer(device: &ash::Device, buffer: &Buffer) {
    // SAFETY: the GPU is done with the buffer, which nothing else holds; freeing the
    // memory unmaps it.
    unsafe {
        device.destroy_buffer(buffer.handle, None);
        device.free_memory(buffer.memory, None);
    }
}

fn make_atlas(
    device: &ash::Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
) -> Result<(vk::Image, vk::DeviceMemory, vk::ImageView), GpuError> {
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: atlas::SIZE,
            height: atlas::SIZE,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    // SAFETY: the device is live.
    let image = unsafe { device.create_image(&info, None) }.during("vkCreateImage")?;
    // SAFETY: the image was just made by this device.
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let kind = memory_type(
        memory,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )
    .or_else(|| {
        memory_type(
            memory,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::empty(),
        )
    })
    .ok_or(GpuError::NoDevice)?;
    let info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(kind);
    // SAFETY: the memory is allocated for this image and bound to it once.
    let memory = unsafe {
        let memory = device
            .allocate_memory(&info, None)
            .during("vkAllocateMemory")?;
        device
            .bind_image_memory(image, memory, 0)
            .during("vkBindImageMemory")?;
        memory
    };
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1),
        );
    // SAFETY: the image is bound to memory.
    let view = unsafe { device.create_image_view(&info, None) }.during("vkCreateImageView")?;
    Ok((image, memory, view))
}

/// The bindings every draw reads: the atlas, its entries, the cells and the frame's quads.
fn make_set_layout(device: &ash::Device) -> Result<vk::DescriptorSetLayout, GpuError> {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(3)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
    ];
    // SAFETY: the device is live and the infos live through the calls below.
    unsafe {
        device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
            None,
        )
    }
    .during("vkCreateDescriptorSetLayout")
}

/// A descriptor set for each of `frames` frames, from a pool of their own.
fn make_sets(
    device: &ash::Device,
    set_layout: vk::DescriptorSetLayout,
    frames: usize,
) -> Result<(vk::DescriptorPool, Vec<vk::DescriptorSet>), GpuError> {
    let count = frames as u32;
    let sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(count),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(3 * count),
    ];
    // SAFETY: the device is live and the infos live through the call.
    let pool = unsafe {
        device.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default()
                .max_sets(count)
                .pool_sizes(&sizes),
            None,
        )
    }
    .during("vkCreateDescriptorPool")?;
    let layouts = vec![set_layout; frames];
    // SAFETY: the pool has room for one set per frame.
    let sets = unsafe {
        device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts),
        )
    }
    .during("vkAllocateDescriptorSets")?;
    Ok((pool, sets))
}

/// Copies `uploads` into `staging`, emptying it, and says where each goes in the atlas.
fn stage(staging: &Buffer, uploads: &mut Vec<atlas::Upload>) -> Vec<vk::BufferImageCopy> {
    let mut regions = Vec::with_capacity(uploads.len());
    let mut offset = 0u64;
    for upload in uploads.drain(..) {
        // SAFETY: the staging buffer is mapped, holds every upload, and the GPU is
        // done with this frame's.
        unsafe {
            std::ptr::copy_nonoverlapping(
                upload.pixels.as_ptr(),
                staging.mapped.add(offset as usize),
                upload.pixels.len(),
            );
        }
        regions.push(
            vk::BufferImageCopy::default()
                .buffer_offset(offset)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D {
                    x: upload.x as i32,
                    y: upload.y as i32,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width: upload.width,
                    height: upload.height,
                    depth: 1,
                }),
        );
        offset += upload.pixels.len() as u64;
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTENT: vk::Extent2D = vk::Extent2D {
        width: 100,
        height: 200,
    };

    fn pane() -> Constants {
        Constants {
            cell_width: 8,
            cell_height: 10,
            origin_x: 20,
            origin_y: 50,
            ..Constants::default()
        }
    }

    /// Each rectangle's top and bottom.
    fn spans(lines: &[bool], reach: Reach) -> Vec<(i32, i32)> {
        changed_rects(lines.iter().copied(), &pane(), reach, EXTENT)
            .iter()
            .inspect(|rect| {
                assert_eq!((rect.offset.x, rect.extent.width), (0, 100));
            })
            .map(|rect| (rect.offset.y, rect.offset.y + rect.extent.height as i32))
            .collect()
    }

    #[test]
    fn each_changed_row_is_one_rectangle_and_neighbours_join() {
        let reach = Reach::default();
        assert_eq!(spans(&[false, false], reach), []);
        assert_eq!(spans(&[false, true, false], reach), [(60, 70)]);
        assert_eq!(
            spans(&[true, true, false, true], reach),
            [(50, 70), (80, 90)]
        );
    }

    #[test]
    fn a_row_takes_in_what_its_glyphs_can_reach() {
        let mut reach = Reach::default();
        // Three pixels above the cell, and six below one that is ten tall.
        let halves = |low: i32, high: i32| (low as u32 & 0xffff) | ((high as u32) << 16);
        reach.take_in(&GpuGlyph {
            offset: halves(0, -3),
            size: halves(8, 19),
            ..GpuGlyph::default()
        });
        assert_eq!(spans(&[false, true], reach), [(57, 76)]);
        // What the rows around it reach joins them.
        assert_eq!(spans(&[true, false, true], reach), [(47, 66), (67, 86)]);
    }

    #[test]
    fn rows_past_the_image_are_cut_off_at_its_edge() {
        let reach = Reach {
            above: 60,
            below: 400,
        };
        assert_eq!(spans(&[true], reach), [(0, 200)]);
    }

    #[test]
    fn only_the_cursor_can_change_without_all_of_a_pane_changing() {
        let moved = Constants {
            cursor_column: 4,
            cursor_line: 2,
            cursor_shape: 1,
            cursor_color: 7,
            first: 100,
            pass: 1,
            ..pane()
        };
        assert_eq!(pane().without_cursor(), moved.without_cursor());
        let elsewhere = Constants {
            origin_x: 21,
            ..pane()
        };
        assert_ne!(pane().without_cursor(), elsewhere.without_cursor());
    }
}
