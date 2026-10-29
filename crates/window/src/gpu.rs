//! The Vulkan device a window draws with, and the swapchain its frames go to.

#![expect(unsafe_code, reason = "Vulkan is only reached through ash's raw calls")]

use std::ffi::c_void;

use ash::{khr, vk};

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("loading Vulkan: {0}")]
    Load(#[from] ash::LoadingError),
    #[error("{call}: {result}")]
    Call {
        call: &'static str,
        result: vk::Result,
    },
    #[error("no device can draw to the window with Vulkan 1.3")]
    NoDevice,
}

trait During<T> {
    fn during(self, call: &'static str) -> Result<T, GpuError>;
}

impl<T> During<T> for Result<T, vk::Result> {
    fn during(self, call: &'static str) -> Result<T, GpuError> {
        self.map_err(|result| GpuError::Call { call, result })
    }
}

/// Frames recorded while earlier ones are still drawn.
const FRAMES_IN_FLIGHT: usize = 2;

struct Frame {
    commands: vk::CommandBuffer,
    acquired: vk::Semaphore,
    done: vk::Fence,
}

struct Swapchain {
    handle: vk::SwapchainKHR,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    /// One for each image, signalled when its frame is drawn: presenting waits on it.
    drawn: Vec<vk::Semaphore>,
}

pub struct Gpu {
    _entry: ash::Entry,
    instance: ash::Instance,
    surfaces: khr::surface::Instance,
    surface: vk::SurfaceKHR,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    swapchains: khr::swapchain::Device,
    format: vk::SurfaceFormatKHR,
    present_mode: vk::PresentModeKHR,
    alpha: vk::CompositeAlphaFlagsKHR,
    pool: vk::CommandPool,
    frames: Vec<Frame>,
    frame: usize,
    swapchain: Option<Swapchain>,
    /// The swapchain does not match the surface, and is made again before the next
    /// frame.
    stale: bool,
}

impl Gpu {
    /// A device that draws to the Wayland surface `surface` of `display`.
    ///
    /// # Safety
    ///
    /// `display` is a live `wl_display` and `surface` a `wl_surface` of it that outlives
    /// the returned value.
    pub unsafe fn new(display: *mut c_void, surface: *mut c_void) -> Result<Self, GpuError> {
        // SAFETY: loading the system's Vulkan loader runs its initialisation, which is
        // what any Vulkan program does.
        let entry = unsafe { ash::Entry::load()? };
        let application = vk::ApplicationInfo::default()
            .application_name(c"Caret")
            .api_version(vk::API_VERSION_1_3);
        let extensions = [
            khr::surface::NAME.as_ptr(),
            khr::wayland_surface::NAME.as_ptr(),
        ];
        let info = vk::InstanceCreateInfo::default()
            .application_info(&application)
            .enabled_extension_names(&extensions);
        // SAFETY: the create info and what it points to live through the call.
        let instance = unsafe { entry.create_instance(&info, None) }.during("vkCreateInstance")?;

        let surfaces = khr::surface::Instance::new(&entry, &instance);
        let wayland = khr::wayland_surface::Instance::new(&entry, &instance);
        let info = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(display)
            .surface(surface);
        // SAFETY: the caller keeps the display and surface alive.
        let surface = match unsafe { wayland.create_wayland_surface(&info, None) } {
            Ok(surface) => surface,
            Err(result) => {
                // SAFETY: nothing was made from the instance.
                unsafe { instance.destroy_instance(None) };
                return Err(GpuError::Call {
                    call: "vkCreateWaylandSurfaceKHR",
                    result,
                });
            }
        };

        let chosen = match choose_device(&instance, &surfaces, surface) {
            Ok(chosen) => chosen,
            Err(error) => {
                // SAFETY: the surface is the only thing made from the instance.
                unsafe {
                    surfaces.destroy_surface(surface, None);
                    instance.destroy_instance(None);
                }
                return Err(error);
            }
        };
        let (physical, family) = chosen;

        let priorities = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let extensions = [khr::swapchain::NAME.as_ptr()];
        let mut vulkan13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        let info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extensions)
            .push_next(&mut vulkan13);
        // SAFETY: the physical device came from this instance, and the infos live
        // through the call.
        let device =
            unsafe { instance.create_device(physical, &info, None) }.during("vkCreateDevice")?;
        // SAFETY: the family was asked for with one queue.
        let queue = unsafe { device.get_device_queue(family, 0) };
        let swapchains = khr::swapchain::Device::new(&instance, &device);

        let (format, present_mode, alpha) = surface_choices(&surfaces, physical, surface)?;
        let (pool, frames) = make_frames(&device, family)?;

        Ok(Self {
            _entry: entry,
            instance,
            surfaces,
            surface,
            physical,
            device,
            queue,
            swapchains,
            format,
            present_mode,
            alpha,
            pool,
            frames,
            frame: 0,
            swapchain: None,
            stale: true,
        })
    }

    /// Draws a frame `width` by `height` pixels, cleared to `background`, premultiplied.
    ///
    /// # Errors
    ///
    /// When the device is lost or out of memory.
    pub fn draw(&mut self, width: u32, height: u32, background: [f32; 4]) -> Result<(), GpuError> {
        let wanted = vk::Extent2D { width, height };
        if self.stale
            || self
                .swapchain
                .as_ref()
                .is_none_or(|chain| chain.extent != wanted)
        {
            self.make_swapchain(wanted)?;
        }
        let Some(swapchain) = self.swapchain.as_ref() else {
            return Ok(());
        };
        let frame = &self.frames[self.frame];
        let device = &self.device;
        // SAFETY: the fence is this device's.
        unsafe { device.wait_for_fences(&[frame.done], true, u64::MAX) }
            .during("vkWaitForFences")?;
        // SAFETY: the swapchain and semaphore are this device's, and the semaphore is
        // unsignalled: its last acquire was waited on by the frame the fence covers.
        let acquired = unsafe {
            self.swapchains.acquire_next_image(
                swapchain.handle,
                u64::MAX,
                frame.acquired,
                vk::Fence::null(),
            )
        };
        let index = match acquired {
            Ok((index, suboptimal)) => {
                self.stale |= suboptimal;
                index
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.stale = true;
                return Ok(());
            }
            Err(result) => {
                return Err(GpuError::Call {
                    call: "vkAcquireNextImageKHR",
                    result,
                });
            }
        };
        let image = swapchain.images[index as usize];
        let view = swapchain.views[index as usize];
        let drawn = swapchain.drawn[index as usize];
        let commands = frame.commands;

        // SAFETY: the fence was waited on, so the GPU is done with the frame.
        unsafe { device.reset_fences(&[frame.done]) }.during("vkResetFences")?;
        record(device, commands, image, view, swapchain.extent, background)?;

        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(frame.acquired)
            .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
        let signals = [vk::SemaphoreSubmitInfo::default()
            .semaphore(drawn)
            .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
        let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(commands)];
        let submit = [vk::SubmitInfo2::default()
            .wait_semaphore_infos(&waits)
            .command_buffer_infos(&buffers)
            .signal_semaphore_infos(&signals)];
        // SAFETY: the command buffer is recorded, and the fence was reset above.
        unsafe { device.queue_submit2(self.queue, &submit, frame.done) }
            .during("vkQueueSubmit2")?;

        let wait = [drawn];
        let chains = [swapchain.handle];
        let indices = [index];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&chains)
            .image_indices(&indices);
        // SAFETY: the image was acquired from this swapchain and is drawn when `drawn`
        // is signalled.
        match unsafe { self.swapchains.queue_present(self.queue, &present) } {
            Ok(suboptimal) => self.stale |= suboptimal,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale = true,
            Err(result) => {
                return Err(GpuError::Call {
                    call: "vkQueuePresentKHR",
                    result,
                });
            }
        }
        self.frame = (self.frame + 1) % FRAMES_IN_FLIGHT;
        Ok(())
    }

    fn make_swapchain(&mut self, wanted: vk::Extent2D) -> Result<(), GpuError> {
        // SAFETY: the surface and the physical device are this instance's.
        let capabilities = unsafe {
            self.surfaces
                .get_physical_device_surface_capabilities(self.physical, self.surface)
        }
        .during("vkGetPhysicalDeviceSurfaceCapabilitiesKHR")?;
        // Wayland leaves the size to the client, which says so with this extent.
        let extent = if capabilities.current_extent.width == u32::MAX {
            vk::Extent2D {
                width: wanted.width.clamp(
                    capabilities.min_image_extent.width,
                    capabilities.max_image_extent.width,
                ),
                height: wanted.height.clamp(
                    capabilities.min_image_extent.height,
                    capabilities.max_image_extent.height,
                ),
            }
        } else {
            capabilities.current_extent
        };
        let mut count = capabilities.min_image_count + 1;
        if capabilities.max_image_count != 0 {
            count = count.min(capabilities.max_image_count);
        }
        let old = self.swapchain.take();
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(self.format.format)
            .image_color_space(self.format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(self.alpha)
            .present_mode(self.present_mode)
            .clipped(true)
            .old_swapchain(
                old.as_ref()
                    .map_or(vk::SwapchainKHR::null(), |old| old.handle),
            );
        // SAFETY: the surface is this instance's, and the old swapchain is retired by
        // passing it here.
        let created = unsafe { self.swapchains.create_swapchain(&info, None) };
        if let Some(old) = old {
            self.destroy_swapchain(old)?;
        }
        let handle = created.during("vkCreateSwapchainKHR")?;
        // SAFETY: the swapchain was just made by this device.
        let images = unsafe { self.swapchains.get_swapchain_images(handle) }
            .during("vkGetSwapchainImagesKHR")?;
        let mut swapchain = Swapchain {
            handle,
            extent,
            images: Vec::new(),
            views: Vec::with_capacity(images.len()),
            drawn: Vec::with_capacity(images.len()),
        };
        for &image in &images {
            let info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(self.format.format)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            // SAFETY: the image belongs to the swapchain just made.
            let view = unsafe { self.device.create_image_view(&info, None) }
                .during("vkCreateImageView")?;
            swapchain.views.push(view);
            // SAFETY: the device is live.
            let drawn = unsafe {
                self.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .during("vkCreateSemaphore")?;
            swapchain.drawn.push(drawn);
        }
        swapchain.images = images;
        self.swapchain = Some(swapchain);
        self.stale = false;
        Ok(())
    }

    fn destroy_swapchain(&self, swapchain: Swapchain) -> Result<(), GpuError> {
        // Its images and semaphores may still be in use by frames being drawn.
        // SAFETY: the device is live.
        unsafe { self.device.device_wait_idle() }.during("vkDeviceWaitIdle")?;
        // SAFETY: the device is idle, so nothing uses these any more.
        unsafe {
            for view in swapchain.views {
                self.device.destroy_image_view(view, None);
            }
            for semaphore in swapchain.drawn {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.swapchains.destroy_swapchain(swapchain.handle, None);
        }
        Ok(())
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(swapchain) = self.swapchain.take()
            && let Err(error) = self.destroy_swapchain(swapchain)
        {
            tracing::warn!("destroying the swapchain: {error}");
        }
        // SAFETY: the device is idle after waiting, and everything below was made from
        // it or from the instance, which go last.
        unsafe {
            let _ = self.device.device_wait_idle();
            for frame in self.frames.drain(..) {
                self.device.destroy_semaphore(frame.acquired, None);
                self.device.destroy_fence(frame.done, None);
            }
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.surfaces.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

/// The device that draws to `surface` best, and its queue family that draws and
/// presents.
fn choose_device(
    instance: &ash::Instance,
    surfaces: &khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32), GpuError> {
    // SAFETY: the instance is live.
    let devices =
        unsafe { instance.enumerate_physical_devices() }.during("vkEnumeratePhysicalDevices")?;
    let mut best = None;
    for physical in devices {
        // SAFETY: the physical device came from this instance.
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        if properties.api_version < vk::API_VERSION_1_3 {
            continue;
        }
        // SAFETY: as above.
        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let family = families.iter().enumerate().find_map(|(index, family)| {
            let index = index as u32;
            // SAFETY: the index is one of the device's families, the surface this
            // instance's.
            let presents =
                unsafe { surfaces.get_physical_device_surface_support(physical, index, surface) }
                    .unwrap_or(false);
            (family.queue_flags.contains(vk::QueueFlags::GRAPHICS) && presents).then_some(index)
        });
        let Some(family) = family else { continue };
        let rank = match properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 0,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
            vk::PhysicalDeviceType::CPU => 4,
            _ => 3,
        };
        if best.is_none_or(|(best_rank, _, _)| rank < best_rank) {
            best = Some((rank, physical, family));
        }
    }
    best.map(|(_, physical, family)| (physical, family))
        .ok_or(GpuError::NoDevice)
}

/// The command pool and what each frame in flight records and waits with.
fn make_frames(
    device: &ash::Device,
    family: u32,
) -> Result<(vk::CommandPool, Vec<Frame>), GpuError> {
    let info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .queue_family_index(family);
    // SAFETY: the device is live.
    let pool = unsafe { device.create_command_pool(&info, None) }.during("vkCreateCommandPool")?;
    let info = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(FRAMES_IN_FLIGHT as u32);
    // SAFETY: the pool is this device's.
    let buffers =
        unsafe { device.allocate_command_buffers(&info) }.during("vkAllocateCommandBuffers")?;
    let mut frames = Vec::with_capacity(FRAMES_IN_FLIGHT);
    for commands in buffers {
        // SAFETY: the device is live; the infos live through the calls.
        let acquired =
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
                .during("vkCreateSemaphore")?;
        let info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: as above.
        let done = unsafe { device.create_fence(&info, None) }.during("vkCreateFence")?;
        frames.push(Frame {
            commands,
            acquired,
            done,
        });
    }
    Ok((pool, frames))
}

/// Records into `commands` a frame that clears `image` to `background` and leaves it
/// ready to present.
fn record(
    device: &ash::Device,
    commands: vk::CommandBuffer,
    image: vk::Image,
    view: vk::ImageView,
    extent: vk::Extent2D,
    background: [f32; 4],
) -> Result<(), GpuError> {
    // SAFETY: the caller waited for the GPU to finish with the command buffer.
    unsafe {
        device
            .reset_command_buffer(commands, vk::CommandBufferResetFlags::empty())
            .during("vkResetCommandBuffer")?;
        device
            .begin_command_buffer(
                commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .during("vkBeginCommandBuffer")?;
    }
    let range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);
    let to_drawing = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .image(image)
        .subresource_range(range)];
    let attachments = [vk::RenderingAttachmentInfo::default()
        .image_view(view)
        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .clear_value(vk::ClearValue {
            color: vk::ClearColorValue {
                float32: background,
            },
        })];
    let rendering = vk::RenderingInfo::default()
        .render_area(vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        })
        .layer_count(1)
        .color_attachments(&attachments);
    let to_present = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
        .src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
        .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
        .image(image)
        .subresource_range(range)];
    // SAFETY: the command buffer is recording, and every handle is this device's.
    unsafe {
        device.cmd_pipeline_barrier2(
            commands,
            &vk::DependencyInfo::default().image_memory_barriers(&to_drawing),
        );
        device.cmd_begin_rendering(commands, &rendering);
        device.cmd_end_rendering(commands);
        device.cmd_pipeline_barrier2(
            commands,
            &vk::DependencyInfo::default().image_memory_barriers(&to_present),
        );
        device
            .end_command_buffer(commands)
            .during("vkEndCommandBuffer")?;
    }
    Ok(())
}

/// The pixel format, present mode and alpha the swapchains of `surface` are made with.
fn surface_choices(
    surfaces: &khr::surface::Instance,
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> Result<
    (
        vk::SurfaceFormatKHR,
        vk::PresentModeKHR,
        vk::CompositeAlphaFlagsKHR,
    ),
    GpuError,
> {
    // SAFETY: the surface and the physical device are this instance's.
    let formats = unsafe { surfaces.get_physical_device_surface_formats(physical, surface) }
        .during("vkGetPhysicalDeviceSurfaceFormatsKHR")?;
    // Colours are blended as the sRGB values programs give, like every terminal does,
    // so the image is UNORM and nothing converts them.
    let format = formats
        .iter()
        .copied()
        .find(|format| {
            matches!(
                format.format,
                vk::Format::B8G8R8A8_UNORM | vk::Format::R8G8B8A8_UNORM
            ) && format.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .or_else(|| formats.first().copied())
        .ok_or(GpuError::NoDevice)?;
    // SAFETY: as above.
    let modes = unsafe { surfaces.get_physical_device_surface_present_modes(physical, surface) }
        .during("vkGetPhysicalDeviceSurfacePresentModesKHR")?;
    // Frames are paced by the compositor's frame callbacks, so presenting never needs
    // to wait for one; FIFO is what every device has.
    let present_mode = if modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    };
    // SAFETY: as above.
    let capabilities =
        unsafe { surfaces.get_physical_device_surface_capabilities(physical, surface) }
            .during("vkGetPhysicalDeviceSurfaceCapabilitiesKHR")?;
    let alpha = [
        vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
        vk::CompositeAlphaFlagsKHR::OPAQUE,
        vk::CompositeAlphaFlagsKHR::INHERIT,
    ]
    .into_iter()
    .find(|alpha| capabilities.supported_composite_alpha.contains(*alpha))
    .unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);
    Ok((format, present_mode, alpha))
}
