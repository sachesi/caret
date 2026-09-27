//! Draws a frame of quads with OpenGL into a texture GTK composites: one instanced draw
//! of rectangles and glyphs from an atlas, into a texture the size of the widget in
//! device pixels.
//!
//! GtkGLArea is not used because it sizes its buffer by the integer scale, which would
//! leave text resampled on a fractional one.

use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use glow::HasContext;

use crate::fonts::{Bitmap, GlyphKey, GlyphText};
use crate::gdk;
use crate::gdk::prelude::*;
use crate::glib;

/// What a quad draws; the fragment shader branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Solid = 0,
    Mask = 1,
    Color = 2,
    Undercurl = 3,
    Dotted = 4,
    Dashed = 5,
}

/// Instances as the vertex shader reads them: rectangle and atlas rectangle as four floats
/// each, the colour as four bytes, the kind as a float.
#[derive(Default)]
pub struct Quads {
    bytes: Vec<u8>,
    count: usize,
}

const QUAD_SIZE: i32 = 40;

impl Quads {
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.count = 0;
    }

    pub fn rect(&mut self, rect: [f32; 4], color: [u8; 4], kind: Kind) {
        self.push(rect, [0.0; 4], color, kind);
    }

    pub fn push(&mut self, rect: [f32; 4], uv: [f32; 4], color: [u8; 4], kind: Kind) {
        let mut quad = [0; QUAD_SIZE as usize];
        for (bytes, value) in quad.chunks_exact_mut(4).zip(rect.into_iter().chain(uv)) {
            bytes.copy_from_slice(&value.to_ne_bytes());
        }
        quad[32..36].copy_from_slice(&color);
        quad[36..].copy_from_slice(&(kind as u8 as f32).to_ne_bytes());
        self.bytes.extend_from_slice(&quad);
        self.count += 1;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasGlyph {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
    pub color: bool,
}

/// Where an ASCII glyph in one of the four styles is kept in the atlas's table.
fn ascii_index(key: &GlyphKey) -> Option<usize> {
    match key.text {
        GlyphText::Char(c) if c.is_ascii() && !key.wide => {
            Some(usize::from(key.style & 3) * 128 + c as usize)
        }
        _ => None,
    }
}

/// The atlas has no room left for a glyph a frame needs.
#[derive(Debug)]
pub struct AtlasFull;

struct Atlas {
    texture: glow::Texture,
    size: i32,
    /// Glyphs are packed in rows: where the next goes, and the height of the row.
    x: i32,
    y: i32,
    row: i32,
    glyphs: HashMap<GlyphKey, Option<AtlasGlyph>>,
    /// ASCII in each style, most of what a frame draws, looked up without hashing.
    ascii: Vec<Option<Option<AtlasGlyph>>>,
}

impl Atlas {
    fn allocate(&mut self, width: i32, height: i32) -> Option<(i32, i32)> {
        if width > self.size || height > self.size {
            return None;
        }
        if self.x + width > self.size {
            self.y += self.row;
            self.x = 0;
            self.row = 0;
        }
        if self.y + height > self.size {
            return None;
        }
        let place = (self.x, self.y);
        self.x += width;
        self.row = self.row.max(height);
        Some(place)
    }
}

/// A texture a frame is drawn into, lent to GTK until it lets go of it.
struct Target {
    texture: glow::Texture,
    width: i32,
    height: i32,
    free: Arc<AtomicBool>,
    sync: Option<glow::Fence>,
    shown: glib::WeakRef<gdk::GLTexture>,
}

pub struct Renderer {
    gl: glow::Context,
    es: bool,
    program: glow::Program,
    vertex_array: glow::VertexArray,
    buffer: glow::Buffer,
    framebuffer: glow::Framebuffer,
    viewport: Option<glow::UniformLocation>,
    atlas_size: Option<glow::UniformLocation>,
    atlas: Atlas,
    max_texture_size: i32,
    targets: Vec<Target>,
}

const VERTEX_SHADER: &str = r"
layout(location = 0) in vec4 rect;
layout(location = 1) in vec4 uv;
layout(location = 2) in vec4 color;
layout(location = 3) in float kind;

uniform vec2 viewport;
uniform vec2 atlas_size;

out vec2 texel;
out vec4 tint;
out vec2 local;
out float band;
flat out int shape;

void main() {
    vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    vec2 position = rect.xy + corner * rect.zw;
    texel = (uv.xy + corner * uv.zw) / atlas_size;
    tint = color;
    local = corner * rect.zw;
    band = rect.w;
    shape = int(kind + 0.5);
    // Row 0 of the texture is the top of the terminal, as GDK reads it.
    gl_Position = vec4(position / viewport * 2.0 - 1.0, 0.0, 1.0);
}
";

const FRAGMENT_SHADER: &str = r"
uniform sampler2D atlas;

in vec2 texel;
in vec4 tint;
in vec2 local;
in float band;
flat in int shape;

out vec4 fragment;

void main() {
    float coverage = 1.0;
    if (shape == 1) {
        coverage = texture(atlas, texel).a;
    } else if (shape == 2) {
        fragment = texture(atlas, texel) * tint.a;
        return;
    } else if (shape == 3) {
        float thickness = max(band / 3.0, 1.0);
        float wave = band * 0.5 + (band - thickness) * 0.5 * sin(gl_FragCoord.x * 6.2831853 / (band * 2.5));
        coverage = clamp(thickness * 0.5 + 0.5 - abs(local.y - wave), 0.0, 1.0);
    } else if (shape == 4) {
        coverage = mod(floor(gl_FragCoord.x / band), 2.0) < 1.0 ? 1.0 : 0.0;
    } else if (shape == 5) {
        coverage = mod(floor(gl_FragCoord.x / (band * 3.0)), 2.0) < 1.0 ? 1.0 : 0.0;
    }
    float alpha = tint.a * coverage;
    fragment = vec4(tint.rgb * alpha, alpha);
}
";

/// OpenGL functions, looked up through EGL: GDK draws through EGL on Wayland and by
/// default on X11, and with libglvnd the functions EGL hands out dispatch to whatever
/// context is current.
fn load() -> Result<glow::Context, String> {
    type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;
    static GET_PROC_ADDRESS: OnceLock<Result<GetProcAddress, String>> = OnceLock::new();
    let get = GET_PROC_ADDRESS.get_or_init(|| {
        // SAFETY: libEGL's initialisers are safe to run; GTK has usually loaded it already.
        let library = unsafe { libloading::Library::new("libEGL.so.1") }
            .map_err(|error| format!("loading libEGL.so.1: {error}"))?;
        // SAFETY: eglGetProcAddress has this signature in every EGL version.
        let get = unsafe { library.get::<GetProcAddress>(b"eglGetProcAddress\0") }
            .map(|symbol| *symbol)
            .map_err(|error| format!("finding eglGetProcAddress: {error}"))?;
        // The function pointer must outlive every context, so the library stays loaded.
        std::mem::forget(library);
        Ok(get)
    });
    let get = *get.as_ref().map_err(Clone::clone)?;
    let loader = |name: &std::ffi::CStr| {
        // SAFETY: `name` is a NUL-terminated function name, all eglGetProcAddress reads.
        unsafe { get(name.as_ptr()) }
    };
    // SAFETY: a context is current, and every pointer the loader returns is either null or
    // the function of that name.
    Ok(unsafe { glow::Context::from_loader_function_cstr(loader) })
}

impl Renderer {
    /// Sets up the program and buffers in `context`, which must be current.
    pub fn new(context: &gdk::GLContext) -> Result<Self, String> {
        let gl = load()?;
        let es = context.api() == gdk::GLAPI::GLES;
        let header = if es {
            "#version 300 es\nprecision highp float;\n"
        } else {
            "#version 330 core\n"
        };
        // SAFETY: `context` is current and every object is created in it; failures are
        // checked before the objects are used.
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, source) in [
                (glow::VERTEX_SHADER, VERTEX_SHADER),
                (glow::FRAGMENT_SHADER, FRAGMENT_SHADER),
            ] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, &format!("{header}{source}"));
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(format!(
                        "compiling a shader: {}",
                        gl.get_shader_info_log(shader)
                    ));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            if !gl.get_program_link_status(program) {
                return Err(format!(
                    "linking the shaders: {}",
                    gl.get_program_info_log(program)
                ));
            }

            let vertex_array = gl.create_vertex_array()?;
            let buffer = gl.create_buffer()?;
            gl.bind_vertex_array(Some(vertex_array));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
            for (location, offset) in [(0, 0), (1, 16)] {
                gl.enable_vertex_attrib_array(location);
                gl.vertex_attrib_pointer_f32(location, 4, glow::FLOAT, false, QUAD_SIZE, offset);
                gl.vertex_attrib_divisor(location, 1);
            }
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 4, glow::UNSIGNED_BYTE, true, QUAD_SIZE, 32);
            gl.vertex_attrib_divisor(2, 1);
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 1, glow::FLOAT, false, QUAD_SIZE, 36);
            gl.vertex_attrib_divisor(3, 1);
            gl.bind_vertex_array(None);

            gl.use_program(Some(program));
            let sampler = gl.get_uniform_location(program, "atlas");
            gl.uniform_1_i32(sampler.as_ref(), 0);
            let viewport = gl.get_uniform_location(program, "viewport");
            let atlas_size = gl.get_uniform_location(program, "atlas_size");

            let max_texture_size = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE).min(8192);
            let size = 1024.min(max_texture_size);
            let atlas = Atlas {
                texture: new_texture(&gl, size, size, false)?,
                size,
                x: 0,
                y: 0,
                row: 0,
                glyphs: HashMap::new(),
                ascii: vec![None; 4 * 128],
            };
            let framebuffer = gl.create_framebuffer()?;

            Ok(Self {
                gl,
                es,
                program,
                vertex_array,
                buffer,
                framebuffer,
                viewport,
                atlas_size,
                atlas,
                max_texture_size,
                targets: Vec::new(),
            })
        }
    }

    /// Where `key`'s glyph is in the atlas, when it was uploaded; inside, nothing for a
    /// glyph without ink.
    pub fn glyph(&self, key: &GlyphKey) -> Option<Option<AtlasGlyph>> {
        match ascii_index(key) {
            Some(index) => self.atlas.ascii[index],
            None => self.atlas.glyphs.get(key).copied(),
        }
    }

    /// Uploads `key`'s glyph to the atlas and says where it went.
    pub fn insert_glyph(
        &mut self,
        key: &GlyphKey,
        bitmap: Option<Bitmap>,
    ) -> Result<Option<AtlasGlyph>, AtlasFull> {
        let glyph = match bitmap {
            None => None,
            Some(bitmap) => {
                let (x, y) = self
                    .atlas
                    .allocate(bitmap.width, bitmap.height)
                    .ok_or(AtlasFull)?;
                // SAFETY: the context is current; the pixels are width × height RGBA rows
                // with no padding, which UNPACK_ALIGNMENT 4 reads as they are.
                unsafe {
                    self.gl
                        .bind_texture(glow::TEXTURE_2D, Some(self.atlas.texture));
                    self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
                    self.gl.tex_sub_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        x,
                        y,
                        bitmap.width,
                        bitmap.height,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(&bitmap.pixels)),
                    );
                }
                Some(AtlasGlyph {
                    x,
                    y,
                    width: bitmap.width,
                    height: bitmap.height,
                    left: bitmap.left,
                    top: bitmap.top,
                    color: bitmap.color,
                })
            }
        };
        match ascii_index(key) {
            Some(index) => self.atlas.ascii[index] = Some(glyph),
            None => {
                self.atlas.glyphs.insert(key.clone(), glyph);
            }
        }
        Ok(glyph)
    }

    /// Forgets every glyph, after the font changed or the atlas filled up; a full atlas
    /// grows first, while the GPU allows.
    pub fn clear_glyphs(&mut self, grow: bool) {
        self.atlas.glyphs.clear();
        self.atlas.ascii.fill(None);
        self.atlas.x = 0;
        self.atlas.y = 0;
        self.atlas.row = 0;
        let size = (self.atlas.size * 2).min(self.max_texture_size);
        if grow && size > self.atlas.size {
            // SAFETY: the context is current and the texture was made in it.
            unsafe {
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(self.atlas.texture));
                allocate(&self.gl, size, size, false);
            }
            self.atlas.size = size;
        }
    }

    /// Draws `quads` over `background` into a `width` by `height` texture for GTK.
    pub fn render(
        &mut self,
        context: &gdk::GLContext,
        width: i32,
        height: i32,
        background: [u8; 3],
        quads: &Quads,
    ) -> Result<gdk::Texture, String> {
        let index = self.target(width, height)?;
        let gl = &self.gl;
        let [r, g, b] = background.map(|channel| f32::from(channel) / 255.0);
        // SAFETY: the context is current and every object bound was made in it; the
        // buffer holds `quads.count` instances of `QUAD_SIZE` bytes each.
        let fence = unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(self.targets[index].texture),
                0,
            );
            gl.viewport(0, 0, width, height);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.clear_color(r, g, b, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            if quads.count > 0 {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                gl.use_program(Some(self.program));
                gl.uniform_2_f32(self.viewport.as_ref(), width as f32, height as f32);
                let atlas = self.atlas.size as f32;
                gl.uniform_2_f32(self.atlas_size.as_ref(), atlas, atlas);
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas.texture));
                gl.bind_vertex_array(Some(self.vertex_array));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.buffer));
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &quads.bytes, glow::STREAM_DRAW);
                gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, quads.count as i32);
                gl.bind_vertex_array(None);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            let fence = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0)?;
            gl.flush();
            fence
        };

        let target = &mut self.targets[index];
        target.sync = Some(fence);
        target.free.store(false, Ordering::SeqCst);
        let free = target.free.clone();
        let format = if self.es {
            gdk::MemoryFormat::R8g8b8a8Premultiplied
        } else {
            gdk::MemoryFormat::B8g8r8a8Premultiplied
        };
        let builder = gdk::GLTextureBuilder::new()
            .set_context(Some(context))
            .set_id(target.texture.0.get())
            .set_width(width)
            .set_height(height)
            .set_format(format)
            .set_sync(Some(fence.0.cast()));
        // SAFETY: the texture stays alive and unchanged until the release function has run,
        // since `target` is not drawn into again before `free` is set.
        let texture =
            unsafe { builder.build_with_release_func(move || free.store(true, Ordering::SeqCst)) };
        if let Some(gl_texture) = texture.downcast_ref::<gdk::GLTexture>() {
            target.shown.set(Some(gl_texture));
        }
        Ok(texture)
    }

    /// A target GTK is done with, of the right size if there is one.
    fn target(&mut self, width: i32, height: i32) -> Result<usize, String> {
        let free: Vec<usize> = (0..self.targets.len())
            .filter(|&i| self.targets[i].free.load(Ordering::SeqCst))
            .collect();
        let chosen = free
            .iter()
            .copied()
            .find(|&i| self.targets[i].width == width && self.targets[i].height == height)
            .or_else(|| free.first().copied());
        let gl = &self.gl;
        // SAFETY: the context is current; GTK has let go of a free target's texture and
        // fence.
        unsafe {
            let index = match chosen {
                Some(index) => index,
                None => {
                    self.targets.push(Target {
                        texture: new_texture(gl, width, height, !self.es)?,
                        width,
                        height,
                        free: Arc::new(AtomicBool::new(true)),
                        sync: None,
                        shown: glib::WeakRef::new(),
                    });
                    self.targets.len() - 1
                }
            };
            let target = &mut self.targets[index];
            if let Some(sync) = target.sync.take() {
                gl.delete_sync(sync);
            }
            if target.width != width || target.height != height {
                gl.bind_texture(glow::TEXTURE_2D, Some(target.texture));
                allocate(gl, width, height, !self.es);
                target.width = width;
                target.height = height;
            }
            Ok(index)
        }
    }

    /// Frees everything the renderer made in its context, which must be current. Frames
    /// GTK still holds are copied out of the GPU first.
    pub fn destroy(self) {
        for target in &self.targets {
            if !target.free.load(Ordering::SeqCst)
                && let Some(texture) = target.shown.upgrade()
            {
                texture.release();
            }
        }
        let gl = &self.gl;
        // SAFETY: the context is current and every object was made in it; none is used
        // after this.
        unsafe {
            for target in &self.targets {
                if let Some(sync) = target.sync {
                    gl.delete_sync(sync);
                }
                gl.delete_texture(target.texture);
            }
            gl.delete_texture(self.atlas.texture);
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_buffer(self.buffer);
            gl.delete_vertex_array(self.vertex_array);
            gl.delete_program(self.program);
        }
    }
}

/// A texture that `allocate` has given storage, read back as it was written.
///
/// # Safety
///
/// A context is current.
unsafe fn new_texture(
    gl: &glow::Context,
    width: i32,
    height: i32,
    bgra: bool,
) -> Result<glow::Texture, String> {
    // SAFETY: the caller made a context current.
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::NEAREST as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::NEAREST as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        allocate(gl, width, height, bgra);
        Ok(texture)
    }
}

/// Storage for the bound texture. Desktop GL describes GDK's textures as BGRA, as
/// GtkGLArea does, and GLES as RGBA.
///
/// # Safety
///
/// A context is current with a texture bound to `TEXTURE_2D`.
unsafe fn allocate(gl: &glow::Context, width: i32, height: i32, bgra: bool) {
    let format = if bgra { glow::BGRA } else { glow::RGBA };
    // SAFETY: the caller bound a texture in the current context; no pixels are read.
    unsafe {
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            width,
            height,
            0,
            format,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
    }
}
