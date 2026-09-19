//! A native capability-scoped WGPU frame proof.
//!
//! It intentionally supports only the bounded raw Zarr v3 subset implemented by `newvolim-io`.
//! The command assembles an authorized local, HTTPS, or S3 OME-Zarr volume, resolves every sample
//! through the same packed static-page table selected by Stage-0, and writes opacity plus an
//! optional OME-coloured projection.

use std::{env, fs, path::PathBuf, sync::mpsc, time::Instant};

use newvolim_io::{
    level_transform, read_v3_raw_u16_volume, DatasetSource, LocalSourcePolicy, RemoteSourcePolicy,
    S3SourcePolicy, SourceRegistry, MAX_REMOTE_ASSET_BYTES,
};
use newvolim_render::{
    NativeLayerDescriptor, NativePortableCameraDrawInput, NativePortableDrawInput,
    NativePortableFrameInput, NativePortableSceneCameraDrawInput, NativePortableSceneInput,
    NativePortableVolumeInput, PortableAnnotationPrimitive, PortableAnnotationPrimitiveKind,
    PortableCameraControls, PortableChannelTransfer, PortablePageSubmission, PortablePageUpload,
    PortableScalarType, ProjectedAnnotationVertex,
};
use newvolim_scene::{LayerId, LayerTransform};

const PAGE_COUNT: u32 = 4;
const PAGE_BYTES: u64 = 4 * 1024 * 1024;
const OFFSET_BITS: u32 = 20;
const PAGE_WORDS: usize = PAGE_BYTES as usize / std::mem::size_of::<u32>();
/// The first scene pass has a fixed work bound per physical camera ray. Inputs that exceed it
/// must choose a coarser level or a smaller admitted region rather than silently truncating.
pub const MAX_PORTABLE_SCENE_RAY_STEPS: u32 = 4_096;
/// Four admitted layers can each retain four independently transferred channels. This is a
/// record-table limit, not an implicit promise that their payloads evade the shared four-page
/// residency bound.
pub const MAX_PORTABLE_SCENE_CHANNELS: usize = PAGE_COUNT as usize * PAGE_COUNT as usize;

/// Exact storage-buffer and uniform uploads consumed by the bounded scene compute pass. Keeping
/// this packet host-testable prevents its layer/channel ordinal mapping from becoming shader-only
/// behaviour.
#[derive(Debug, PartialEq)]
pub struct PortableSceneGpuPacket {
    pub config: [u32; 200],
    pub page_table: Vec<u32>,
    pub rays: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Arguments {
    source: SourceArgument,
    output: PathBuf,
    color_output: Option<PathBuf>,
    depth_output: Option<PathBuf>,
    timing_output: Option<PathBuf>,
    warm_iterations: u32,
    level: String,
    axis: RayAxis,
    annotation_fixture: bool,
}

/// The one-channel OME display policy carried into the renderer target. `color_linear` is
/// premultiplied by opacity in the shader; display encoding happens only at PPM readback.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TransferFunction {
    color_linear: [f32; 3],
    color_srgb: [u8; 3],
    window: [f32; 2],
    opacity: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceArgument {
    Local(PathBuf),
    Https {
        root: String,
        allowed_hosts: Vec<String>,
    },
    S3 {
        bucket: String,
        prefix: String,
        profile: String,
        region: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RayAxis {
    Z,
    Y,
    X,
}

impl RayAxis {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "z" | "Z" => Ok(Self::Z),
            "y" | "Y" => Ok(Self::Y),
            "x" | "X" => Ok(Self::X),
            _ => Err(format!("--axis must be x, y, or z; got {value:?}")),
        }
    }

    pub fn output_dimensions(self, [width, height, depth]: [usize; 3]) -> [usize; 2] {
        match self {
            Self::Z => [width, height],
            Self::Y => [width, depth],
            Self::X => [height, depth],
        }
    }

    fn shader_value(self) -> u32 {
        match self {
            Self::Z => 0,
            Self::Y => 1,
            Self::X => 2,
        }
    }
}

fn main() -> Result<(), String> {
    let arguments = parse_arguments(env::args().skip(1))?;
    let load_started = Instant::now();
    let source = open_source(&arguments.source)?;
    let level = resolve_level(&source, &arguments.level)?;
    let volume = read_v3_raw_u16_volume(&source, &level, PAGE_BYTES * u64::from(PAGE_COUNT))
        .map_err(|error| format!("failed to load raw v3 volume: {error}"))?;
    let load_ms = load_started.elapsed().as_secs_f64() * 1_000.0;
    let [depth, height, width] = volume.dimensions_zyx;
    let dimensions_xyz = [width, height, depth];
    let [frame_width, frame_height] = arguments.axis.output_dimensions(dimensions_xyz);
    let ray_step = physical_ray_step(&source, &level, arguments.axis)?;
    let transfer = transfer_function(&source)?;
    let annotation_words = if arguments.annotation_fixture {
        fixture_annotation_words(frame_width, frame_height)
    } else {
        Vec::new()
    };
    let frame = if volume.voxels.len() <= PAGE_WORDS {
        let direct = direct_volume_input(&volume.voxels, dimensions_xyz, transfer)?;
        let draw = NativePortableDrawInput::new(
            direct,
            [frame_width as u32, frame_height as u32],
            PortableCameraControls::new([0, 0], 1.0).map_err(|error| error.to_string())?,
            annotation_words,
        )
        .map_err(|error| error.to_string())?;
        render_portable_draw(&draw, arguments.axis, ray_step, arguments.warm_iterations)?
    } else {
        render_projection(
            &volume.voxels,
            dimensions_xyz,
            ProjectionRequest {
                axis: arguments.axis,
                ray_step,
                transfer,
                annotation_words: &annotation_words,
                camera_rays: None,
                frame_extent: None,
                warm_iterations: arguments.warm_iterations,
            },
        )?
    };
    let pgm = encode_pgm(frame_width, frame_height, &frame.pixels)?;
    fs::write(&arguments.output, pgm)
        .map_err(|error| format!("failed to write {}: {error}", arguments.output.display()))?;
    if let Some(color_output) = &arguments.color_output {
        fs::write(
            color_output,
            encode_ppm(frame_width, frame_height, &frame.rgba)?,
        )
        .map_err(|error| format!("failed to write {}: {error}", color_output.display()))?;
    }
    if let Some(depth_output) = &arguments.depth_output {
        fs::write(
            depth_output,
            encode_pfm(frame_width, frame_height, &frame.ray_distances)?,
        )
        .map_err(|error| format!("failed to write {}: {error}", depth_output.display()))?;
    }
    if let Some(timing_output) = &arguments.timing_output {
        fs::write(
            timing_output,
            frame.timing.as_json(load_ms, frame_width, frame_height),
        )
        .map_err(|error| format!("failed to write {}: {error}", timing_output.display()))?;
    }
    if arguments.warm_iterations != 0 {
        println!(
            "native wgpu warm dispatch: {:.3} ms mean across {} completed dispatches (no readback)",
            frame.timing.warm_dispatch_ms / f64::from(arguments.warm_iterations),
            arguments.warm_iterations,
        );
    }
    if let Some(color_output) = &arguments.color_output {
        println!(
            "native wgpu colour target: {} ({}×{} sRGB PPM from OME channel 0)",
            color_output.display(),
            frame_width,
            frame_height,
        );
    }
    let nonzero = frame.pixels.iter().filter(|&&pixel| pixel != 0).count();
    println!(
        "native wgpu frame: {} (level {}, {}×{}×{} XYZ uint16) -> {} ({}×{} PGM, {nonzero} nonzero pixels)",
        source_description(&arguments.source),
        level,
        width,
        height,
        depth,
        arguments.output.display(),
        frame_width,
        frame_height,
    );
    if let Some(depth_output) = &arguments.depth_output {
        println!(
            "native wgpu ray distance: {} ({}×{} PFM, physical units, +∞ means no opacity hit)",
            depth_output.display(),
            frame_width,
            frame_height,
        );
    }
    if let Some(timing_output) = &arguments.timing_output {
        println!(
            "native wgpu timing JSON: {} (cold phases plus optional warm dispatches)",
            timing_output.display(),
        );
    }
    println!(
        "native wgpu timing: load {load_ms:.3} ms; adapter/device {:.3} ms; setup/upload {:.3} ms; dispatch/readback {:.3} ms",
        frame.timing.adapter_device_ms,
        frame.timing.setup_upload_ms,
        frame.timing.dispatch_readback_ms,
    );
    Ok(())
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Arguments, String> {
    let mut root = None;
    let mut https_root = None;
    let mut s3_bucket = None;
    let mut s3_prefix = None;
    let mut s3_profile = None;
    let mut s3_region = None;
    let mut allowed_hosts = Vec::new();
    let mut output = None;
    let mut color_output = None;
    let mut depth_output = None;
    let mut timing_output = None;
    let mut warm_iterations = 0;
    let mut level = "0".to_owned();
    let mut axis = RayAxis::Z;
    let mut annotation_fixture = false;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        if flag == "--annotation-fixture" {
            if annotation_fixture {
                return Err("--annotation-fixture may be specified only once".into());
            }
            annotation_fixture = true;
            continue;
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--zarr" if root.is_none() => root = Some(PathBuf::from(value)),
            "--https-root" if https_root.is_none() => https_root = Some(value),
            "--s3-bucket" if s3_bucket.is_none() => s3_bucket = Some(value),
            "--s3-prefix" if s3_prefix.is_none() => s3_prefix = Some(value),
            "--s3-profile" if s3_profile.is_none() => s3_profile = Some(value),
            "--s3-region" if s3_region.is_none() => s3_region = Some(value),
            "--allow-host" => allowed_hosts.push(value),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--color-output" if color_output.is_none() => color_output = Some(PathBuf::from(value)),
            "--depth-output" if depth_output.is_none() => depth_output = Some(PathBuf::from(value)),
            "--timing-output" if timing_output.is_none() => timing_output = Some(PathBuf::from(value)),
            "--warm-iterations" if warm_iterations == 0 => {
                warm_iterations = value.parse::<u32>().map_err(|_| "--warm-iterations must be a positive u32".to_owned())?;
                if warm_iterations == 0 {
                    return Err("--warm-iterations must be positive".into());
                }
            }
            "--level" if !value.is_empty() => level = value,
            "--axis" => axis = RayAxis::parse(&value)?,
            "--zarr" | "--https-root" | "--s3-bucket" | "--s3-prefix" | "--s3-profile"
            | "--s3-region" | "--output" | "--color-output" | "--depth-output" | "--timing-output" | "--warm-iterations" => return Err(format!("{flag} may be specified only once")),
            "--level" => {
                return Err("--level requires `auto` or a non-empty Zarr-relative array path".into())
            }
            _ => return Err(format!("unknown option {flag}; use (--zarr ROOT | --https-root URL --allow-host HOST | --s3-bucket BUCKET --s3-prefix PREFIX --s3-profile PROFILE [--s3-region REGION]) --output FRAME.pgm [--color-output FRAME.ppm] [--depth-output DEPTH.pfm] [--timing-output TIMING.json] [--warm-iterations N] [--level LEVEL] [--axis x|y|z] [--annotation-fixture]")),
        }
    }
    let source = match (root, https_root, s3_bucket) {
        (Some(root), None, None) => {
            if !allowed_hosts.is_empty() {
                return Err("--allow-host is only valid with --https-root".into());
            }
            if s3_prefix.is_some() || s3_profile.is_some() || s3_region.is_some() {
                return Err("S3 options require --s3-bucket".into());
            }
            SourceArgument::Local(root)
        }
        (None, Some(root), None) => {
            if allowed_hosts.is_empty() {
                return Err("--https-root requires at least one explicit --allow-host".into());
            }
            if s3_prefix.is_some() || s3_profile.is_some() || s3_region.is_some() {
                return Err("S3 options require --s3-bucket".into());
            }
            SourceArgument::Https {
                root,
                allowed_hosts,
            }
        }
        (None, None, Some(bucket)) => {
            if !allowed_hosts.is_empty() {
                return Err("--allow-host is only valid with --https-root".into());
            }
            SourceArgument::S3 {
                bucket,
                prefix: s3_prefix.ok_or_else(|| "--s3-bucket requires --s3-prefix".to_owned())?,
                profile: s3_profile
                    .ok_or_else(|| "--s3-bucket requires --s3-profile".to_owned())?,
                region: s3_region,
            }
        }
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => {
            return Err("choose exactly one of --zarr, --https-root, or --s3-bucket".into())
        }
        (None, None, None) => {
            return Err("--zarr ROOT, --https-root URL, or --s3-bucket BUCKET is required".into())
        }
    };
    Ok(Arguments {
        source,
        output: output.ok_or_else(|| "--output FRAME.pgm is required".to_owned())?,
        color_output,
        depth_output,
        timing_output,
        warm_iterations,
        level,
        axis,
        annotation_fixture,
    })
}

/// A deliberately small recorder diagnostic: a red point in front of the volume and a green
/// point at the same pixel behind it. It proves that the production frame shader consumes the
/// same fixed packet layout and first-opacity depth comparison as desktop/browser; ordinary
/// callers always provide their projected scene records instead.
fn fixture_annotation_words(frame_width: usize, frame_height: usize) -> Vec<u32> {
    let center = [frame_width as f32 * 0.5, frame_height as f32 * 0.5];
    let visible = PortableAnnotationPrimitive {
        kind: PortableAnnotationPrimitiveKind::Point,
        annotation_id: 1,
        color_srgb: [255, 0, 0],
        radius: 2.0,
        vertices: [ProjectedAnnotationVertex {
            pixel: center,
            ray_distance: 0.0,
        }; 3],
    };
    let occluded = PortableAnnotationPrimitive {
        kind: PortableAnnotationPrimitiveKind::Point,
        annotation_id: 2,
        color_srgb: [0, 255, 0],
        radius: 2.0,
        vertices: [ProjectedAnnotationVertex {
            pixel: center,
            ray_distance: f32::MAX,
        }; 3],
    };
    [visible.words(), occluded.words()]
        .into_iter()
        .flatten()
        .collect()
}

/// Select the first OME channel for the one-channel spike. Invalid or absent channel metadata
/// has a deterministic red/full-range fallback instead of silently inventing a random palette.
fn transfer_function(source: &DatasetSource) -> Result<TransferFunction, String> {
    let metadata = source.read_dataset_metadata().map_err(|error| {
        format!("could not read OME-Zarr metadata for colour transfer: {error}")
    })?;
    let channel = metadata
        .omero
        .and_then(|omero| omero.channels.into_iter().next());
    let color_srgb = channel
        .as_ref()
        .and_then(|channel| channel.color.as_deref())
        .and_then(parse_srgb_hex_bytes)
        .unwrap_or([0xf2, 0x28, 0x1f]);
    let color_linear = color_srgb.map(|component| srgb_to_linear(f32::from(component) / 255.0));
    let window = channel
        .and_then(|channel| channel.window)
        .filter(|window| {
            window.start.is_finite() && window.end.is_finite() && window.end > window.start
        })
        .map(|window| [window.start as f32, window.end as f32])
        .filter(|window| window[0].is_finite() && window[1].is_finite())
        .unwrap_or([0.0, 65535.0]);
    Ok(TransferFunction {
        color_linear,
        color_srgb,
        window,
        opacity: 1.0,
    })
}

fn parse_srgb_hex_bytes(value: &str) -> Option<[u8; 3]> {
    if value.len() != 6 || !value.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

/// Render a validated one-to-four-page portable draw packet through the native bounded WGPU pass.
///
/// This is intentionally a library entry point as well as the CLI's direct-volume route: desktop
/// and headless callers can submit the exact packet admitted by `newvolim-render`, including its
/// annotation stream, without serializing it through a command-line-only format.
pub fn render_portable_draw(
    input: &NativePortableDrawInput,
    axis: RayAxis,
    ray_step: f32,
    warm_iterations: u32,
) -> Result<RenderedProjection, String> {
    // The axis recorder is the legacy/default route. A fitted Palace camera must be consumed via
    // the ray-table recorder rather than silently producing a frame from unrelated controls.
    if input.camera.orbit_delta != [0, 0] || input.camera.zoom != 1.0 {
        return Err(
            "portable draw has fitted camera controls; submit its per-pixel ray table to the ray recorder"
                .into(),
        );
    }
    let volume = &input.volume;
    if volume.scalar_type != PortableScalarType::Uint16 {
        return Err(
            "the native WGPU direct-volume path currently accepts uint16 pages only".into(),
        );
    }
    let channels = portable_volume_u16_channels(volume)?;
    let dimensions_xyz = volume.dimensions_xyz.map(|dimension| dimension as usize);
    let output_dimensions = axis.output_dimensions(dimensions_xyz);
    if input.extent_pixels
        != [
            u32::try_from(output_dimensions[0]).map_err(|_| "direct draw width exceeds u32")?,
            u32::try_from(output_dimensions[1]).map_err(|_| "direct draw height exceeds u32")?,
        ]
    {
        return Err("direct draw extent does not match its selected orthogonal projection".into());
    }
    render_projection_channels(
        &channels,
        dimensions_xyz,
        ProjectionRequest {
            axis,
            ray_step,
            transfer: channels[0].transfer,
            annotation_words: &input.annotation_words,
            camera_rays: None,
            frame_extent: None,
            warm_iterations,
        },
    )
}

/// Render a camera-complete portable packet using the exact per-pixel rays that projected its
/// annotations. Unlike the legacy axis route, this accepts arbitrary fitted Palace controls.
pub fn render_portable_camera_draw(
    input: &NativePortableCameraDrawInput,
    warm_iterations: u32,
) -> Result<RenderedProjection, String> {
    let volume = &input.draw.volume;
    if volume.scalar_type != PortableScalarType::Uint16 {
        return Err(
            "the native WGPU direct-volume path currently accepts uint16 pages only".into(),
        );
    }
    let channels = portable_volume_u16_channels(volume)?;
    let dimensions_xyz = volume.dimensions_xyz.map(|dimension| dimension as usize);
    let extent = input.draw.extent_pixels.map(|value| value as usize);
    render_projection_channels(
        &channels,
        dimensions_xyz,
        ProjectionRequest {
            axis: RayAxis::Z,
            ray_step: 1.0,
            transfer: channels[0].transfer,
            annotation_words: &input.draw.annotation_words,
            camera_rays: Some(&input.rays),
            frame_extent: Some(extent),
            warm_iterations,
        },
    )
}

/// Render an ordered, transformed portable scene from its one-authority physical world-ray
/// table. The fixed four-page residency bound and the 4,096-step per-ray limit were validated
/// before this function obtains an adapter.
pub fn render_portable_scene_camera_draw(
    input: &NativePortableSceneCameraDrawInput,
    warm_iterations: u32,
) -> Result<RenderedProjection, String> {
    let packet = prepare_portable_scene_gpu_packet(input)?;
    let [frame_width, frame_height] = input.draw.extent_pixels.map(|value| value as usize);
    let pixels = frame_width
        .checked_mul(frame_height)
        .ok_or_else(|| "portable scene frame dimensions overflow usize".to_owned())?;
    let started = Instant::now();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|error| format!("no WGPU adapter: {error}"))?;
    if adapter.limits().max_storage_buffers_per_shader_stage < PAGE_COUNT + 4 {
        return Err(
            "adapter does not expose enough storage buffers for portable scene rendering".into(),
        );
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("newvolim native WGPU scene"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .map_err(|error| format!("request_device failed: {error}"))?;
    let adapter_device_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let upload_started = Instant::now();
    let storage_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    let pages: Vec<_> = (0..PAGE_COUNT)
        .map(|page| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("newvolim scene static page {page}")),
                size: PAGE_BYTES,
                usage: storage_usage,
                mapped_at_creation: false,
            })
        })
        .collect();
    for (page, words) in input
        .draw
        .scene
        .frame
        .page_submission
        .pages
        .iter()
        .enumerate()
    {
        queue.write_buffer(&pages[page], 0, &bytes_of_u32(words));
    }
    let page_table = storage_buffer(
        &device,
        "newvolim scene page table",
        &packet.page_table,
        false,
    );
    queue.write_buffer(&page_table, 0, &bytes_of_u32(&packet.page_table));
    let rays = storage_buffer(&device, "newvolim scene world rays", &packet.rays, false);
    queue.write_buffer(&rays, 0, &bytes_of_u32(&packet.rays));
    let config = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim scene config"),
        size: std::mem::size_of_val(&packet.config) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&config, 0, &bytes_of_u32(&packet.config));
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim scene RGBA16F"),
        size: (pixels * 8) as u64,
        usage: storage_usage | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let output_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim scene RGBA16F readback"),
        size: (pixels * 8) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let distance = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim scene ray distance"),
        size: (pixels * 4) as u64,
        usage: storage_usage | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let distance_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim scene ray distance readback"),
        size: (pixels * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("newvolim scene layout"),
        entries: &[
            storage_layout(0, false),
            storage_layout(1, true),
            storage_layout(2, true),
            storage_layout(3, true),
            storage_layout(4, true),
            storage_layout(5, true),
            uniform_layout(6),
            storage_layout(7, false),
            storage_layout(8, true),
        ],
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("newvolim scene bind group"),
        layout: &layout,
        entries: &[
            buffer_entry(0, &output),
            buffer_entry(1, &page_table),
            buffer_entry(2, &pages[0]),
            buffer_entry(3, &pages[1]),
            buffer_entry(4, &pages[2]),
            buffer_entry(5, &pages[3]),
            buffer_entry(6, &config),
            buffer_entry(7, &distance),
            buffer_entry(8, &rays),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("newvolim scene pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("newvolim scene shader"),
        source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("newvolim scene raymarch"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let setup_upload_ms = upload_started.elapsed().as_secs_f64() * 1_000.0;
    let dispatch_started = Instant::now();
    let dispatch = |encoder: &mut wgpu::CommandEncoder| {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            (frame_width as u32).div_ceil(8),
            (frame_height as u32).div_ceil(8),
            1,
        );
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    dispatch(&mut encoder);
    encoder.copy_buffer_to_buffer(&output, 0, &output_readback, 0, (pixels * 8) as u64);
    encoder.copy_buffer_to_buffer(&distance, 0, &distance_readback, 0, (pixels * 4) as u64);
    queue.submit([encoder.finish()]);
    let read = |buffer: &wgpu::Buffer| -> Result<Vec<u8>, String> {
        let (sender, receiver) = mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap()
            });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let bytes = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|error| error.to_string())?
            .to_vec();
        buffer.unmap();
        Ok(bytes)
    };
    let rgba_bytes = read(&output_readback)?;
    let distance_bytes = read(&distance_readback)?;
    let rgba = rgba_bytes
        .as_chunks::<4>()
        .0
        .as_chunks::<2>()
        .0
        .iter()
        .map(|words| {
            let rg = unpack_f16_pair(u32::from_ne_bytes(words[0]));
            let ba = unpack_f16_pair(u32::from_ne_bytes(words[1]));
            [
                linear_to_srgb_byte(rg[0]),
                linear_to_srgb_byte(rg[1]),
                linear_to_srgb_byte(ba[0]),
                (ba[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect::<Vec<_>>();
    let ray_distances = distance_bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_ne_bytes(*word))
        .collect::<Vec<_>>();
    let warm_started = Instant::now();
    for _ in 0..warm_iterations {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        dispatch(&mut encoder);
        queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| error.to_string())?;
    }
    Ok(RenderedProjection {
        pixels: rgba.iter().map(|pixel| pixel[3]).collect(),
        rgba,
        ray_distances,
        timing: RenderTiming {
            adapter_device_ms,
            setup_upload_ms,
            dispatch_readback_ms: dispatch_started.elapsed().as_secs_f64() * 1_000.0,
            warm_iterations,
            warm_dispatch_ms: warm_started.elapsed().as_secs_f64() * 1_000.0,
        },
    })
}

struct ProjectionChannel {
    voxels: Vec<u16>,
    transfer: TransferFunction,
}

fn portable_scene_f32_bits(value: f64, field: &str) -> Result<u32, String> {
    let narrowed = value as f32;
    if !narrowed.is_finite() {
        return Err(format!(
            "portable scene {field} is outside the native f32 range"
        ));
    }
    Ok(narrowed.to_bits())
}

/// Fixed 64-byte layer records for the future native scene raymarch shader. The record is kept
/// separate from the current one-volume frame uniform so scene page ownership cannot be inferred
/// from a channel's ordinal position.
pub fn pack_portable_scene_layers(scene: &NativePortableSceneInput) -> Result<[u32; 64], String> {
    if scene.layers.len() > PAGE_COUNT as usize {
        return Err("portable scene exceeds four layer records".into());
    }
    let mut packed = [0_u32; 64];
    let mut channel_table_offset = 0_u32;
    for (index, layer) in scene.layers.iter().enumerate() {
        if layer.scalar_type != PortableScalarType::Uint16 {
            return Err("native portable scene currently accepts uint16 layers only".into());
        }
        if layer.channels.is_empty() || layer.channels.len() > PAGE_COUNT as usize {
            return Err("portable scene layer has an unsupported channel count".into());
        }
        let voxel_origin = layer
            .voxel_origin_xyz
            .map(u32::try_from)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "portable scene voxel origin exceeds GPU record range".to_owned())?;
        let base = index * 16;
        packed[base..base + 8].copy_from_slice(&[
            layer.dimensions_xyz[0],
            layer.dimensions_xyz[1],
            layer.dimensions_xyz[2],
            layer.channels.len() as u32,
            voxel_origin[0],
            voxel_origin[1],
            voxel_origin[2],
            channel_table_offset,
        ]);
        packed[base + 8..base + 14].copy_from_slice(&[
            portable_scene_f32_bits(layer.transform.scale[0], "layer scale")?,
            portable_scene_f32_bits(layer.transform.scale[1], "layer scale")?,
            portable_scene_f32_bits(layer.transform.scale[2], "layer scale")?,
            portable_scene_f32_bits(layer.transform.translation[0], "layer translation")?,
            portable_scene_f32_bits(layer.transform.translation[1], "layer translation")?,
            portable_scene_f32_bits(layer.transform.translation[2], "layer translation")?,
        ]);
        channel_table_offset = channel_table_offset
            .checked_add(layer.channels.len() as u32)
            .ok_or_else(|| "portable scene channel table offset overflows u32".to_owned())?;
    }
    Ok(packed)
}

/// Fixed eight-word records for the channel table referenced by [`pack_portable_scene_layers`].
/// Page ranges stay absolute in the common four-page pool, while colour is narrowed once to
/// linear `f32` for the native shader.
pub fn pack_portable_scene_channels(
    scene: &NativePortableSceneInput,
) -> Result<[u32; MAX_PORTABLE_SCENE_CHANNELS * 8], String> {
    let mut packed = [0_u32; MAX_PORTABLE_SCENE_CHANNELS * 8];
    let mut record = 0_usize;
    for layer in &scene.layers {
        for channel in &layer.channels {
            if record == MAX_PORTABLE_SCENE_CHANNELS {
                return Err("portable scene exceeds its channel record capacity".into());
            }
            let transfer = channel.transfer;
            if !transfer.opacity.is_finite() || !(0.0..=1.0).contains(&transfer.opacity) {
                return Err("portable scene channel has invalid opacity".into());
            }
            let base = record * 8;
            packed[base..base + 8].copy_from_slice(&[
                channel.page_offset,
                channel.page_count,
                portable_scene_f32_bits(transfer.window_start, "channel window")?,
                portable_scene_f32_bits(transfer.window_end, "channel window")?,
                transfer.opacity.to_bits(),
                srgb_to_linear(f32::from(transfer.color_srgb[0]) / 255.0).to_bits(),
                srgb_to_linear(f32::from(transfer.color_srgb[1]) / 255.0).to_bits(),
                srgb_to_linear(f32::from(transfer.color_srgb[2]) / 255.0).to_bits(),
            ]);
            record += 1;
        }
    }
    Ok(packed)
}

/// Compute the finite physical-world interval that each camera ray needs to march through the
/// admitted scene. A miss is represented by an empty `[0, -1]` interval; it is explicit in the
/// GPU upload and never mistaken for a zero-distance hit.
pub fn portable_scene_ray_ranges(
    input: &NativePortableSceneCameraDrawInput,
) -> Result<Vec<[f32; 2]>, String> {
    let step = input.draw.scene.world_ray_step();
    if !step.is_finite() || step <= 0.0 {
        return Err("portable scene has an invalid physical ray step".into());
    }
    input
        .rays
        .iter()
        .map(|ray| {
            let interval = input
                .draw
                .scene
                .layers
                .iter()
                .filter_map(|layer| layer.world_ray_interval(*ray))
                .fold(None, |union: Option<(f64, f64)>, (near, far)| {
                    Some(match union {
                        Some((old_near, old_far)) => (old_near.min(near), old_far.max(far)),
                        None => (near, far),
                    })
                });
            let Some((near, far)) = interval else {
                return Ok([0.0, -1.0]);
            };
            let steps = ((far - near) / step).ceil();
            if !steps.is_finite() || steps > f64::from(MAX_PORTABLE_SCENE_RAY_STEPS) {
                return Err(format!(
                    "portable scene ray requires {steps} steps; limit is {MAX_PORTABLE_SCENE_RAY_STEPS}"
                ));
            }
            Ok([
                f32::from_bits(portable_scene_f32_bits(near, "ray interval")?),
                f32::from_bits(portable_scene_f32_bits(far, "ray interval")?),
            ])
        })
        .collect()
}

/// Build the common scene GPU packet: an eight-word header followed by four layer records and
/// sixteen channel records, a flat page-table for every channel's XYZ logical words, and one
/// eight-word world-ray/range record per output pixel.
pub fn prepare_portable_scene_gpu_packet(
    input: &NativePortableSceneCameraDrawInput,
) -> Result<PortableSceneGpuPacket, String> {
    let layer_records = pack_portable_scene_layers(&input.draw.scene)?;
    let channel_records = pack_portable_scene_channels(&input.draw.scene)?;
    let ranges = portable_scene_ray_ranges(input)?;
    let mut config = [0_u32; 200];
    config[..5].copy_from_slice(&[
        input.draw.extent_pixels[0],
        input.draw.extent_pixels[1],
        input.draw.scene.layers.len() as u32,
        portable_scene_f32_bits(input.draw.scene.world_ray_step(), "ray step")?,
        u32::try_from(input.draw.annotation_words.len() / PortableAnnotationPrimitive::WORDS)
            .map_err(|_| "portable scene annotation count overflows u32")?,
    ]);
    config[8..72].copy_from_slice(&layer_records);
    config[72..200].copy_from_slice(&channel_records);
    let mut page_table = Vec::new();
    for layer in &input.draw.scene.layers {
        let words = layer
            .dimensions_xyz
            .iter()
            .try_fold(1_usize, |total, &dimension| {
                total.checked_mul(dimension as usize)
            })
            .ok_or_else(|| "portable scene dimensions overflow page table".to_owned())?;
        for channel in &layer.channels {
            let first = usize::try_from(channel.page_offset)
                .map_err(|_| "portable scene page offset exceeds usize")?
                .checked_mul(PAGE_WORDS)
                .ok_or_else(|| "portable scene page offset overflows".to_owned())?;
            if first
                .checked_add(words)
                .is_none_or(|end| end > PAGE_WORDS * PAGE_COUNT as usize)
            {
                return Err("portable scene channel exceeds static page pool".into());
            }
            page_table.extend((0..words).map(|index| pack_page_location(first + index)));
        }
    }
    let mut rays = input.draw.annotation_words.clone();
    rays.reserve(input.rays.len() * 8);
    for (ray, range) in input.rays.iter().zip(ranges) {
        for value in ray
            .origin_world
            .into_iter()
            .chain(ray.direction_world)
            .chain(range.map(f64::from))
        {
            rays.push(portable_scene_f32_bits(value, "camera ray")?);
        }
    }
    Ok(PortableSceneGpuPacket {
        config,
        page_table,
        rays,
    })
}

fn portable_volume_u16_channels(
    volume: &NativePortableVolumeInput,
) -> Result<Vec<ProjectionChannel>, String> {
    let descriptor = volume
        .frame
        .descriptors
        .first()
        .filter(|_| {
            volume.frame.descriptors.len() == 1 && volume.frame.descriptors[0].page_offset == 0
        })
        .ok_or_else(|| "direct portable volume has no single page-zero descriptor".to_owned())?;
    let _count = usize::try_from(descriptor.page_count)
        .ok()
        .filter(|count| (1..=PAGE_COUNT as usize).contains(count))
        .ok_or_else(|| "direct portable volume has an invalid page count".to_owned())?;
    volume
        .channels
        .iter()
        .map(|channel| {
            if channel.page_offset + channel.page_count > descriptor.page_count {
                return Err("direct portable channel range exceeds its descriptor pages".into());
            }
            let voxels = volume.frame.page_submission.pages
                [channel.page_offset as usize..(channel.page_offset + channel.page_count) as usize]
                .iter()
                .flatten()
                .copied()
                .map(|value| {
                    u16::try_from(value).map_err(|_| {
                        format!(
                            "uint16 direct-volume page contains out-of-range storage word {value}"
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let transfer = TransferFunction {
                color_linear: channel
                    .transfer
                    .color_srgb
                    .map(|component| srgb_to_linear(f32::from(component) / 255.0)),
                color_srgb: channel.transfer.color_srgb,
                window: [
                    channel.transfer.window_start as f32,
                    channel.transfer.window_end as f32,
                ],
                opacity: channel.transfer.opacity,
            };
            if !transfer.window.iter().all(|value| value.is_finite()) {
                return Err("direct-volume native-unit window cannot be represented as f32".into());
            }
            Ok(ProjectionChannel { voxels, transfer })
        })
        .collect()
}

fn direct_volume_input(
    voxels: &[u16],
    dimensions_xyz: [usize; 3],
    transfer: TransferFunction,
) -> Result<NativePortableVolumeInput, String> {
    let dimensions_xyz = [
        u32::try_from(dimensions_xyz[0])
            .map_err(|_| "direct-volume X dimension does not fit portable u32")?,
        u32::try_from(dimensions_xyz[1])
            .map_err(|_| "direct-volume Y dimension does not fit portable u32")?,
        u32::try_from(dimensions_xyz[2])
            .map_err(|_| "direct-volume Z dimension does not fit portable u32")?,
    ];
    let submission = PortablePageSubmission::from_uploads([PortablePageUpload {
        page: 0,
        words: voxels.iter().map(|&voxel| u32::from(voxel)).collect(),
    }])
    .map_err(|error| error.to_string())?;
    let frame = NativePortableFrameInput::new(
        vec![NativeLayerDescriptor {
            layer_id: LayerId(0),
            page_offset: 0,
            page_count: 1,
            transform: LayerTransform::IDENTITY,
        }],
        submission,
    )
    .map_err(|error| error.to_string())?;
    NativePortableVolumeInput::new(
        frame,
        dimensions_xyz,
        PortableScalarType::Uint16,
        PortableChannelTransfer {
            color_srgb: transfer.color_srgb,
            window_start: f64::from(transfer.window[0]),
            window_end: f64::from(transfer.window[1]),
            opacity: transfer.opacity,
        },
    )
    .map_err(|error| error.to_string())
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Length in physical coordinate-system units travelled by one axis-aligned voxel step. The
/// raw v3 reader is deliberately three-dimensional; composing the NGFF affine here means the
/// depth attachment stays valid for anisotropic data and for a rotated physical coordinate frame.
fn physical_ray_step(
    source: &DatasetSource,
    level_path: &str,
    axis: RayAxis,
) -> Result<f32, String> {
    let metadata = source.read_dataset_metadata().map_err(|error| {
        format!("could not read OME-Zarr metadata for physical ray distance: {error}")
    })?;
    let multiscale = metadata
        .multiscales
        .iter()
        .find(|multiscale| {
            multiscale
                .datasets
                .iter()
                .any(|dataset| dataset.path == level_path)
        })
        .ok_or_else(|| format!("level {level_path:?} is not declared by an OME-NGFF multiscale"))?;
    if multiscale.axes.len() != 3 {
        return Err(format!(
            "physical ray distance requires exactly three spatial axes, got {}",
            multiscale.axes.len()
        ));
    }
    let level = multiscale
        .datasets
        .iter()
        .position(|dataset| dataset.path == level_path)
        .expect("multiscale selection established this level exists");
    let coordinate_axis = match axis {
        RayAxis::X => "x",
        RayAxis::Y => "y",
        RayAxis::Z => "z",
    };
    let input_axis = multiscale
        .axes
        .iter()
        .position(|candidate| candidate.name.eq_ignore_ascii_case(coordinate_axis))
        .ok_or_else(|| format!("OME-NGFF multiscale has no {coordinate_axis:?} axis"))?;
    let transform = level_transform(multiscale, level)
        .map_err(|error| format!("invalid OME-NGFF level transform: {error}"))?;
    let squared_length: f64 = transform.matrix()[..3]
        .iter()
        .map(|row| row[input_axis] * row[input_axis])
        .sum();
    let step = squared_length.sqrt();
    if !step.is_finite() || step <= 0.0 || step > f64::from(f32::MAX) {
        return Err(format!(
            "invalid physical ray step {step} for {coordinate_axis} axis"
        ));
    }
    Ok(step as f32)
}

fn open_source(source: &SourceArgument) -> Result<DatasetSource, String> {
    match source {
        SourceArgument::Local(root) => open_local_source(root),
        SourceArgument::Https {
            root,
            allowed_hosts,
        } => open_https_source(root, allowed_hosts),
        SourceArgument::S3 {
            bucket,
            prefix,
            profile,
            region,
        } => open_s3_source(bucket, prefix, profile, region.as_deref()),
    }
}

fn source_description(source: &SourceArgument) -> String {
    match source {
        SourceArgument::Local(root) => root.display().to_string(),
        SourceArgument::Https { root, .. } => root.clone(),
        SourceArgument::S3 { bucket, prefix, .. } => format!("s3://{bucket}/{prefix}"),
    }
}

fn open_local_source(root: &PathBuf) -> Result<DatasetSource, String> {
    let parent = root
        .parent()
        .ok_or_else(|| format!("dataset root {} has no parent directory", root.display()))?;
    let registry = SourceRegistry::new(
        LocalSourcePolicy::new(vec![parent.to_path_buf()])
            .map_err(|error| format!("invalid local source policy: {error}"))?,
        RemoteSourcePolicy::default(),
        MAX_REMOTE_ASSET_BYTES,
    )
    .map_err(|error| format!("invalid source registry: {error}"))?;
    registry
        .open_local(root)
        .map_err(|error| format!("local source rejected: {error}"))
}

fn open_https_source(root: &str, allowed_hosts: &[String]) -> Result<DatasetSource, String> {
    let registry = SourceRegistry::new(
        LocalSourcePolicy::default(),
        RemoteSourcePolicy::new(allowed_hosts.iter().cloned())
            .map_err(|error| format!("invalid HTTPS source policy: {error}"))?,
        MAX_REMOTE_ASSET_BYTES,
    )
    .map_err(|error| format!("invalid source registry: {error}"))?;
    registry
        .open_https(root)
        .map_err(|error| format!("HTTPS source rejected: {error}"))
}

fn open_s3_source(
    bucket: &str,
    prefix: &str,
    profile: &str,
    region: Option<&str>,
) -> Result<DatasetSource, String> {
    let registry = SourceRegistry::new(
        LocalSourcePolicy::default(),
        RemoteSourcePolicy::default(),
        MAX_REMOTE_ASSET_BYTES,
    )
    .map_err(|error| format!("invalid source registry: {error}"))?
    .with_s3_policy(
        S3SourcePolicy::new(vec![bucket.to_owned()], vec![profile.to_owned()])
            .map_err(|error| format!("invalid S3 source policy: {error}"))?,
    );
    registry
        .open_s3(bucket, prefix, profile, region)
        .map_err(|error| format!("S3 source rejected: {error}"))
}

/// Resolve `--level auto` to the finest declared level whose uncompressed uint16 payload fits
/// the fixed portable page pool. Explicit paths retain their exact caller-selected meaning.
fn resolve_level(source: &DatasetSource, requested: &str) -> Result<String, String> {
    if requested != "auto" {
        return Ok(requested.to_owned());
    }
    let metadata = source.read_dataset_metadata().map_err(|error| {
        format!("could not read OME-Zarr metadata for automatic level choice: {error}")
    })?;
    let multiscale = metadata
        .multiscales
        .first()
        .ok_or_else(|| "automatic level choice requires an OME-NGFF multiscale".to_owned())?;
    let pool_bytes = PAGE_BYTES * u64::from(PAGE_COUNT);
    for dataset in &multiscale.datasets {
        let array = source.read_array_info(&dataset.path).map_err(|error| {
            format!(
                "could not read OME-Zarr array metadata for automatic level {}: {error}",
                dataset.path
            )
        })?;
        let bytes = array
            .shape
            .iter()
            .try_fold(2_u64, |bytes, &extent| bytes.checked_mul(extent))
            .ok_or_else(|| format!("automatic level {} byte size overflows u64", dataset.path))?;
        if bytes <= pool_bytes {
            return Ok(dataset.path.clone());
        }
    }
    Err(format!(
        "no declared OME-NGFF level fits the {pool_bytes}-byte portable page pool"
    ))
}

struct ProjectionRequest<'a> {
    axis: RayAxis,
    ray_step: f32,
    transfer: TransferFunction,
    annotation_words: &'a [u32],
    camera_rays: Option<&'a [newvolim_render::PortableCameraRay]>,
    frame_extent: Option<[usize; 2]>,
    warm_iterations: u32,
}

fn render_projection(
    voxels: &[u16],
    dimensions_xyz: [usize; 3],
    request: ProjectionRequest<'_>,
) -> Result<RenderedProjection, String> {
    let channels = [ProjectionChannel {
        voxels: voxels.to_vec(),
        transfer: request.transfer,
    }];
    render_projection_channels(&channels, dimensions_xyz, request)
}

fn render_projection_channels(
    channels: &[ProjectionChannel],
    dimensions_xyz: [usize; 3],
    request: ProjectionRequest<'_>,
) -> Result<RenderedProjection, String> {
    let ProjectionRequest {
        axis,
        ray_step,
        transfer: _,
        annotation_words,
        camera_rays,
        frame_extent,
        warm_iterations,
    } = request;
    let [width, height, depth] = dimensions_xyz;
    let voxel_count = width
        .checked_mul(height)
        .and_then(|count| count.checked_mul(depth))
        .ok_or_else(|| "volume dimensions overflow usize".to_owned())?;
    if channels.is_empty() || channels.len() > PAGE_COUNT as usize {
        return Err(format!(
            "portable direct renderer needs 1..={PAGE_COUNT} channels, got {}",
            channels.len()
        ));
    }
    if channels
        .iter()
        .any(|channel| channel.voxels.len() != voxel_count)
    {
        return Err(format!(
            "portable channel volume dimensions require {voxel_count} values each"
        ));
    }
    if !annotation_words.len().is_multiple_of(13) {
        return Err(format!(
            "annotation packet has {} words; projected primitives require exactly 13 words",
            annotation_words.len()
        ));
    }
    let annotation_count = annotation_words.len() / 13;
    if annotation_count > NativePortableDrawInput::MAX_ANNOTATION_PRIMITIVES {
        return Err(format!(
            "annotation packet has {annotation_count} primitives; portable capacity is {}",
            NativePortableDrawInput::MAX_ANNOTATION_PRIMITIVES
        ));
    }
    let pool_words = PAGE_WORDS * PAGE_COUNT as usize;
    if voxel_count
        .checked_mul(channels.len())
        .is_none_or(|words| words > pool_words)
    {
        return Err(format!(
            "{} channels × {voxel_count} voxels exceed the {PAGE_COUNT}-page portable pool of {pool_words} words",
            channels.len(),
        ));
    }
    let [frame_width, frame_height] =
        frame_extent.unwrap_or_else(|| axis.output_dimensions(dimensions_xyz));
    let frame_pixels = frame_width
        .checked_mul(frame_height)
        .ok_or_else(|| "frame dimensions overflow usize".to_owned())?;
    if let Some(rays) = camera_rays {
        if rays.len() != frame_pixels {
            return Err(format!(
                "camera ray table has {} entries; frame requires {frame_pixels}",
                rays.len()
            ));
        }
    }
    let adapter_device_started = Instant::now();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|error| format!("no WGPU adapter: {error}"))?;
    let limits = adapter.limits();
    if limits.max_storage_buffers_per_shader_stage < PAGE_COUNT + 4 {
        return Err(format!(
            "adapter exposes {} storage buffers/stage; the portable pool needs {}",
            limits.max_storage_buffers_per_shader_stage,
            PAGE_COUNT + 4
        ));
    }
    if limits.max_storage_buffer_binding_size < PAGE_BYTES {
        return Err(format!(
            "adapter storage binding limit {} is below {PAGE_BYTES}",
            limits.max_storage_buffer_binding_size
        ));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("newvolim native WGPU frame"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .map_err(|error| format!("request_device failed: {error}"))?;
    let adapter_device_ms = adapter_device_started.elapsed().as_secs_f64() * 1_000.0;
    let setup_upload_started = Instant::now();
    let storage_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    let pages: Vec<_> = (0..PAGE_COUNT)
        .map(|page| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("newvolim static brick page {page}")),
                size: PAGE_BYTES,
                usage: storage_usage,
                mapped_at_creation: false,
            })
        })
        .collect();
    let mut next_page = 0_usize;
    let mut page_table = Vec::with_capacity(voxel_count * channels.len());
    for channel in channels {
        if !channel
            .transfer
            .window
            .iter()
            .all(|value| value.is_finite())
            || channel.transfer.window[1] < channel.transfer.window[0]
            || !channel.transfer.opacity.is_finite()
            || !(0.0..=1.0).contains(&channel.transfer.opacity)
        {
            return Err("colour transfer has an invalid window or opacity".into());
        }
        let page_count = channel.voxels.len().div_ceil(PAGE_WORDS);
        if next_page
            .checked_add(page_count)
            .is_none_or(|end| end > PAGE_COUNT as usize)
        {
            return Err("direct portable channel pages exceed the fixed four-page pool".into());
        }
        let words: Vec<u32> = channel
            .voxels
            .iter()
            .map(|&voxel| u32::from(voxel))
            .collect();
        for page_offset in 0..page_count {
            let first = page_offset * PAGE_WORDS;
            let last = (first + PAGE_WORDS).min(words.len());
            queue.write_buffer(
                &pages[next_page + page_offset],
                0,
                &bytes_of_u32(&words[first..last]),
            );
        }
        page_table.extend(
            (0..voxel_count).map(|index| pack_page_location(next_page * PAGE_WORDS + index)),
        );
        next_page += page_count;
    }
    let page_table_buffer =
        storage_buffer(&device, "newvolim packed page table", &page_table, true);
    queue.write_buffer(&page_table_buffer, 0, &bytes_of_u32(&page_table));
    // WebGPU bindings may not have a zero-sized range. The explicit count in the uniform makes
    // this harmless placeholder inaccessible when there are no annotations.
    let mut packet_upload = annotation_words.to_vec();
    if let Some(rays) = camera_rays {
        packet_upload.extend(
            rays.iter()
                .flat_map(|ray| ray.origin_xyz.into_iter().chain(ray.direction_xyz))
                .map(f32::to_bits),
        );
    }
    if packet_upload.is_empty() {
        packet_upload.push(0);
    }
    let packet = storage_buffer(
        &device,
        "newvolim portable annotations and camera rays",
        &packet_upload,
        false,
    );
    queue.write_buffer(&packet, 0, &bytes_of_u32(&packet_upload));
    let mut config = [0_u32; 44];
    config[..10].copy_from_slice(&[
        width as u32,
        height as u32,
        depth as u32,
        frame_width as u32,
        frame_height as u32,
        axis.shader_value(),
        ray_step.to_bits(),
        u32::from(camera_rays.is_some()),
        annotation_count as u32,
        channels.len() as u32,
    ]);
    for (index, channel) in channels.iter().enumerate() {
        let base = 12 + index * 8;
        config[base..base + 7].copy_from_slice(&[
            channel.transfer.window[0].to_bits(),
            channel.transfer.window[1].to_bits(),
            channel.transfer.opacity.to_bits(),
            (index * voxel_count) as u32,
            channel.transfer.color_linear[0].to_bits(),
            channel.transfer.color_linear[1].to_bits(),
            channel.transfer.color_linear[2].to_bits(),
        ]);
    }
    let config_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim frame dimensions"),
        size: std::mem::size_of_val(&config) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&config_buffer, 0, &bytes_of_u32(&config));
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim linear RGBA16F projection"),
        // Two packed half-float pairs per pixel: RG then BA.
        size: (frame_pixels * 2 * std::mem::size_of::<u32>()) as u64,
        usage: storage_usage | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim linear RGBA16F projection readback"),
        size: (frame_pixels * 2 * std::mem::size_of::<u32>()) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let ray_distance = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim first-opacity ray distance"),
        size: (frame_pixels * std::mem::size_of::<f32>()) as u64,
        usage: storage_usage | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let ray_distance_readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("newvolim first-opacity ray distance readback"),
        size: (frame_pixels * std::mem::size_of::<f32>()) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("newvolim static-page layout"),
        entries: &[
            storage_layout(0, false),
            storage_layout(1, true),
            storage_layout(2, true),
            storage_layout(3, true),
            storage_layout(4, true),
            storage_layout(5, true),
            uniform_layout(6),
            storage_layout(7, false),
            storage_layout(8, true),
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("newvolim static-page bind group"),
        layout: &layout,
        entries: &[
            buffer_entry(0, &output),
            buffer_entry(1, &page_table_buffer),
            buffer_entry(2, &pages[0]),
            buffer_entry(3, &pages[1]),
            buffer_entry(4, &pages[2]),
            buffer_entry(5, &pages[3]),
            buffer_entry(6, &config_buffer),
            buffer_entry(7, &ray_distance),
            buffer_entry(8, &packet),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("newvolim static-page pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("newvolim raw-volume raymarch shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("newvolim raw-volume raymarch"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let setup_upload_ms = setup_upload_started.elapsed().as_secs_f64() * 1_000.0;
    let dispatch_readback_started = Instant::now();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            (frame_width as u32).div_ceil(8),
            (frame_height as u32).div_ceil(8),
            1,
        );
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, (frame_pixels * 8) as u64);
    encoder.copy_buffer_to_buffer(
        &ray_distance,
        0,
        &ray_distance_readback,
        0,
        (frame_pixels * 4) as u64,
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| error.to_string())?;
    receiver
        .recv()
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let mapped = readback
        .slice(..)
        .get_mapped_range()
        .map_err(|error| error.to_string())?;
    let rgba: Vec<[u8; 4]> = mapped
        .as_chunks::<4>()
        .0
        .as_chunks::<2>()
        .0
        .iter()
        .map(|words| {
            let rg = unpack_f16_pair(u32::from_ne_bytes(words[0]));
            let ba = unpack_f16_pair(u32::from_ne_bytes(words[1]));
            [
                linear_to_srgb_byte(rg[0]),
                linear_to_srgb_byte(rg[1]),
                linear_to_srgb_byte(ba[0]),
                (ba[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();
    drop(mapped);
    readback.unmap();
    let (sender, receiver) = mpsc::channel();
    ray_distance_readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| error.to_string())?;
    receiver
        .recv()
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let mapped = ray_distance_readback
        .slice(..)
        .get_mapped_range()
        .map_err(|error| error.to_string())?;
    let ray_distances = mapped
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_ne_bytes(*word))
        .collect();
    drop(mapped);
    ray_distance_readback.unmap();
    let warm_dispatch_started = Instant::now();
    for _ in 0..warm_iterations {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                (frame_width as u32).div_ceil(8),
                (frame_height as u32).div_ceil(8),
                1,
            );
        }
        queue.submit([encoder.finish()]);
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| error.to_string())?;
    }
    Ok(RenderedProjection {
        pixels: rgba.iter().map(|pixel| pixel[3]).collect(),
        rgba,
        ray_distances,
        timing: RenderTiming {
            adapter_device_ms,
            setup_upload_ms,
            dispatch_readback_ms: dispatch_readback_started.elapsed().as_secs_f64() * 1_000.0,
            warm_iterations,
            warm_dispatch_ms: warm_dispatch_started.elapsed().as_secs_f64() * 1_000.0,
        },
    })
}

pub struct RenderedProjection {
    pub pixels: Vec<u8>,
    pub rgba: Vec<[u8; 4]>,
    pub ray_distances: Vec<f32>,
    timing: RenderTiming,
}

/// Cold-path timing boundaries plus optional completed warm dispatches. The warm series reuses
/// the device, pipeline, bind group, and uploaded pages, but deliberately omits readback.
struct RenderTiming {
    adapter_device_ms: f64,
    setup_upload_ms: f64,
    dispatch_readback_ms: f64,
    warm_iterations: u32,
    warm_dispatch_ms: f64,
}

impl RenderTiming {
    /// Stable, dependency-free JSON for repeated local benchmark collection. Values are cold
    /// phase timings in milliseconds, while frame dimensions document the exact workload.
    fn as_json(&self, load_ms: f64, width: usize, height: usize) -> String {
        format!(
            concat!(
                "{{\"kind\":\"newvolim-native-wgpu-frame\",",
                "\"width\":{width},\"height\":{height},",
                "\"loadMs\":{load_ms:.6},",
                "\"adapterDeviceMs\":{adapter_device_ms:.6},",
                "\"setupUploadMs\":{setup_upload_ms:.6},",
                "\"dispatchReadbackMs\":{dispatch_readback_ms:.6},",
                "\"warmIterations\":{warm_iterations},",
                "\"warmDispatchMs\":{warm_dispatch_ms:.6}}}\n"
            ),
            width = width,
            height = height,
            load_ms = load_ms,
            adapter_device_ms = self.adapter_device_ms,
            setup_upload_ms = self.setup_upload_ms,
            dispatch_readback_ms = self.dispatch_readback_ms,
            warm_iterations = self.warm_iterations,
            warm_dispatch_ms = self.warm_dispatch_ms,
        )
    }
}

pub fn encode_pgm(width: usize, height: usize, pixels: &[u8]) -> Result<Vec<u8>, String> {
    if pixels.len() != width * height {
        return Err("pixel count does not match PGM dimensions".into());
    }
    let mut output = format!("P5\n{width} {height}\n255\n").into_bytes();
    output.extend_from_slice(pixels);
    Ok(output)
}

/// Binary Netpbm colour output. The compute attachment is RGBA8 encoded sRGB; PPM deliberately
/// discards the alpha byte because the grayscale PGM remains the scalar-opacity compatibility
/// output while PFM carries the independent ray-distance attachment.
pub fn encode_ppm(width: usize, height: usize, pixels: &[[u8; 4]]) -> Result<Vec<u8>, String> {
    if pixels.len() != width * height {
        return Err("colour pixel count does not match PPM dimensions".into());
    }
    let mut output = format!("P6\n{width} {height}\n255\n").into_bytes();
    for pixel in pixels {
        output.extend_from_slice(&pixel[..3]);
    }
    Ok(output)
}

fn linear_to_srgb_byte(value: f32) -> u8 {
    let encoded = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Decodes the two IEEE-754 binary16 values produced by WGSL's `pack2x16float`. Keeping the
/// target packed avoids requiring the optional shader-f16 feature while retaining the S8
/// RGBA16Float storage contract.
fn unpack_f16_pair(word: u32) -> [f32; 2] {
    [half_to_f32(word as u16), half_to_f32((word >> 16) as u16)]
}

fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let fraction = u32::from(bits & 0x03ff);
    let output = match exponent {
        0 if fraction == 0 => sign,
        0 => {
            let mut fraction = fraction;
            let mut exponent = -14_i32;
            while fraction & 0x0400 == 0 {
                fraction <<= 1;
                exponent -= 1;
            }
            sign | (((exponent + 127) as u32) << 23) | ((fraction & 0x03ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (fraction << 13),
        _ => sign | ((exponent + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(output)
}

/// Portable Float Map sidecar. A negative scale denotes little-endian f32, preserving `+∞`
/// exactly for rays that never crossed the first-opacity threshold.
pub fn encode_pfm(width: usize, height: usize, distances: &[f32]) -> Result<Vec<u8>, String> {
    if distances.len() != width * height {
        return Err("ray-distance count does not match PFM dimensions".into());
    }
    let mut output = format!("Pf\n{width} {height}\n-1.0\n").into_bytes();
    // PFM stores the bottom raster row first. `ray_distances` follows the top-to-bottom WGPU
    // output index, so reverse rows rather than reversing individual pixels.
    for row in distances.chunks_exact(width).rev() {
        for distance in row {
            output.extend_from_slice(&distance.to_le_bytes());
        }
    }
    Ok(output)
}

fn bytes_of_u32(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect()
}

fn pack_page_location(offset: usize) -> u32 {
    let page = offset / PAGE_WORDS;
    let page_offset = offset % PAGE_WORDS;
    debug_assert!(page < PAGE_COUNT as usize);
    ((page as u32) << OFFSET_BITS) | page_offset as u32
}

fn storage_buffer(
    device: &wgpu::Device,
    label: &'static str,
    values: &[u32],
    copy_src: bool,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (values.len() * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | if copy_src {
                wgpu::BufferUsages::COPY_SRC
            } else {
                wgpu::BufferUsages::empty()
            },
        mapped_at_creation: false,
    })
}

fn storage_layout(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn uniform_layout(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn buffer_entry<'a>(binding: u32, buffer: &'a wgpu::Buffer) -> wgpu::BindGroupEntry<'a> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var<storage, read_write> output: array<u32>;
@group(0) @binding(1) var<storage, read> page_table: array<u32>;
@group(0) @binding(2) var<storage, read> page0: array<u32>;
@group(0) @binding(3) var<storage, read> page1: array<u32>;
@group(0) @binding(4) var<storage, read> page2: array<u32>;
@group(0) @binding(5) var<storage, read> page3: array<u32>;
struct Frame {
  width: u32, height: u32, depth: u32,
  frame_width: u32, frame_height: u32, axis: u32,
  ray_step_bits: u32, ray_mode: u32,
  annotation_count: u32, channel_count: u32, _pad0: u32, _pad1: u32,
  channels: array<Channel, 4>,
}
struct Channel {
  window_start: f32, window_end: f32, opacity: f32, page_table_offset: u32,
  red: f32, green: f32, blue: f32, _pad: u32,
}
@group(0) @binding(6) var<uniform> frame: Frame;
@group(0) @binding(7) var<storage, read_write> ray_distance: array<f32>;
// Thirteen u32 words per projected primitive: kind, stable ID, packed sRGB colour, radius,
// then three screen-space XY/ray-distance vertices. This exporter preserves the stable ID in
// the common packet even though it has no picking attachment of its own.
// First come thirteen-word projected annotations; when ray_mode is set, they are immediately
// followed by origin XYZ and normalized direction XYZ pairs for every physical pixel.
@group(0) @binding(8) var<storage, read> packet: array<u32>;
fn sample(location: u32) -> u32 {
  let page = location >> 20u; let offset = location & 0x000fffffu;
  if (page == 0u) { return page0[offset]; }
  if (page == 1u) { return page1[offset]; }
  if (page == 2u) { return page2[offset]; }
  if (page == 3u) { return page3[offset]; }
  return 0u;
}
fn srgb_to_linear(value: f32) -> f32 {
  if (value <= 0.04045) { return value / 12.92; }
  return pow((value + 0.055) / 1.055, 2.4);
}
fn cross2(a: vec2f, b: vec2f, point: vec2f) -> f32 {
  return (point.x - a.x) * (b.y - a.y) - (point.y - a.y) * (b.x - a.x);
}
struct AnnotationHit { covered: bool, ray_distance: f32, colour: vec3f }
fn annotation_hit(base: u32, pixel: vec2f) -> AnnotationHit {
  let kind = packet[base];
  let packed_colour = packet[base + 2u];
  let encoded_colour = vec3f(
    f32((packed_colour >> 16u) & 255u) / 255.0,
    f32((packed_colour >> 8u) & 255u) / 255.0,
    f32(packed_colour & 255u) / 255.0,
  );
  let colour = vec3f(
    srgb_to_linear(encoded_colour.r), srgb_to_linear(encoded_colour.g), srgb_to_linear(encoded_colour.b),
  );
  let radius = bitcast<f32>(packet[base + 3u]);
  let a = vec2f(bitcast<f32>(packet[base + 4u]), bitcast<f32>(packet[base + 5u]));
  let ad = bitcast<f32>(packet[base + 6u]);
  if (kind == 1u) { return AnnotationHit(distance(pixel, a) <= radius, ad, colour); }
  let b = vec2f(bitcast<f32>(packet[base + 7u]), bitcast<f32>(packet[base + 8u]));
  let bd = bitcast<f32>(packet[base + 9u]);
  if (kind == 2u) {
    let ab = b - a;
    let length_squared = dot(ab, ab);
    if (length_squared == 0.0) { return AnnotationHit(distance(pixel, a) <= radius, ad, colour); }
    let t = clamp(dot(pixel - a, ab) / length_squared, 0.0, 1.0);
    return AnnotationHit(distance(pixel, a + t * ab) <= radius, mix(ad, bd, t), colour);
  }
  if (kind == 3u) {
    let c = vec2f(bitcast<f32>(packet[base + 10u]), bitcast<f32>(packet[base + 11u]));
    let cd = bitcast<f32>(packet[base + 12u]);
    let ab = cross2(a, b, pixel); let bc = cross2(b, c, pixel); let ca = cross2(c, a, pixel);
    let covered = (ab >= 0.0 && bc >= 0.0 && ca >= 0.0) || (ab <= 0.0 && bc <= 0.0 && ca <= 0.0);
    let denominator = cross2(a, b, c);
    if (abs(denominator) <= 0.000001) { return AnnotationHit(false, 0.0, colour); }
    let b_weight = cross2(a, pixel, c) / denominator;
    let c_weight = cross2(a, b, pixel) / denominator;
    return AnnotationHit(covered, ad + b_weight * (bd - ad) + c_weight * (cd - ad), colour);
  }
  return AnnotationHit(false, 0.0, colour);
}
fn slab_interval(origin: f32, direction: f32, lower: f32, upper: f32) -> vec2f {
  if (abs(direction) < 0.0000001) {
    if (origin < lower || origin > upper) { return vec2f(1.0, -1.0); }
    return vec2f(-1e30, 1e30);
  }
  let first = (lower - origin) / direction;
  let second = (upper - origin) / direction;
  return vec2f(min(first, second), max(first, second));
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3u) {
  if (gid.x >= frame.frame_width || gid.y >= frame.frame_height) { return; }
  var opacity = 0.0;
  var colour = vec3f(0.0);
  var ray_length = frame.depth;
  if (frame.axis == 1u) { ray_length = frame.height; }
  if (frame.axis == 2u) { ray_length = frame.width; }
  let output_index = gid.x + frame.frame_width * gid.y;
  var camera_origin = vec3f(0.0);
  var camera_direction = vec3f(0.0, 0.0, 1.0);
  var camera_entry = 0.0;
  var camera_exit = 0.0;
  if (frame.ray_mode == 1u) {
    let base = frame.annotation_count * 13u + output_index * 6u;
    camera_origin = vec3f(bitcast<f32>(packet[base]), bitcast<f32>(packet[base + 1u]), bitcast<f32>(packet[base + 2u]));
    camera_direction = vec3f(bitcast<f32>(packet[base + 3u]), bitcast<f32>(packet[base + 4u]), bitcast<f32>(packet[base + 5u]));
    // Voxel-centred box: voxel i occupies [i - 0.5, i + 0.5), matching NGFF, Palace and the
    // annotation placement convention.
    let x_interval = slab_interval(camera_origin.x, camera_direction.x, -0.5, f32(frame.width) - 0.5);
    let y_interval = slab_interval(camera_origin.y, camera_direction.y, -0.5, f32(frame.height) - 0.5);
    let z_interval = slab_interval(camera_origin.z, camera_direction.z, -0.5, f32(frame.depth) - 0.5);
    camera_entry = max(0.0, max(x_interval.x, max(y_interval.x, z_interval.x)));
    camera_exit = min(x_interval.y, min(y_interval.y, z_interval.y));
    if (camera_exit > camera_entry) {
      ray_length = u32(ceil((camera_exit - camera_entry) / 0.5));
    } else {
      ray_length = 0u;
    }
  }
  ray_distance[output_index] = bitcast<f32>(0x7f800000u);
  for (var ray = 0u; ray < ray_length; ray = ray + 1u) {
    var x = 0u; var y = 0u; var z = 0u;
    var travelled = (f32(ray) + 0.5) * bitcast<f32>(frame.ray_step_bits);
    if (frame.ray_mode == 1u) {
      travelled = camera_entry + (f32(ray) + 0.5) * 0.5;
      if (travelled >= camera_exit) { break; }
      let position = camera_origin + camera_direction * travelled;
      x = u32(clamp(floor(position.x + 0.5), 0.0, f32(frame.width - 1u)));
      y = u32(clamp(floor(position.y + 0.5), 0.0, f32(frame.height - 1u)));
      z = u32(clamp(floor(position.z + 0.5), 0.0, f32(frame.depth - 1u)));
    } else {
      if (frame.axis == 0u) { x = gid.x; y = gid.y; z = ray; }
      if (frame.axis == 1u) { x = gid.x; y = ray; z = gid.y; }
      if (frame.axis == 2u) { x = ray; y = gid.x; z = gid.y; }
    }
    let index = x + frame.width * (y + frame.height * z);
    var sample_colour = vec3f(0.0);
    var sample_opacity = 0.0;
    for (var channel_index = 0u; channel_index < frame.channel_count; channel_index = channel_index + 1u) {
      let channel = frame.channels[channel_index];
      let sample_value = f32(sample(page_table[channel.page_table_offset + index]));
      var intensity = 0.0;
      if (channel.window_start == channel.window_end) {
        intensity = select(0.0, 1.0, sample_value >= channel.window_end);
      } else {
        intensity = clamp(
          (sample_value - channel.window_start) / (channel.window_end - channel.window_start),
          0.0,
          1.0,
        );
      }
      let channel_alpha = intensity * channel.opacity;
      sample_colour = sample_colour + vec3f(channel.red, channel.green, channel.blue) * channel_alpha;
      sample_opacity = min(1.0, sample_opacity + channel_alpha);
    }
    let alpha = sample_opacity * 0.06;
    colour = colour + (1.0 - opacity) * sample_colour * 0.06;
    opacity = opacity + (1.0 - opacity) * alpha;
    if (opacity >= 0.01 && ray_distance[output_index] == bitcast<f32>(0x7f800000u)) {
      ray_distance[output_index] = travelled;
    }
  }
  // The first-opacity attachment carries physical ray units, so this comparison remains valid
  // for anisotropic datasets and projected slanted primitives.
  for (var primitive = 0u; primitive < frame.annotation_count; primitive = primitive + 1u) {
    let hit = annotation_hit(primitive * 13u, vec2f(f32(gid.x), f32(gid.y)));
    if (hit.covered && hit.ray_distance <= ray_distance[output_index]) {
      colour = hit.colour;
      opacity = 1.0;
    }
  }
  let packed_index = output_index * 2u;
  output[packed_index] = pack2x16float(colour.rg);
  output[packed_index + 1u] = pack2x16float(vec2f(colour.b, opacity));
}
"#;

/// Bounded world-space scene raymarch shader. The host-side `PortableSceneGpuPacket` is laid out
/// directly as its `Scene` uniform, page table, and eight-word ray records.
pub const SCENE_SHADER: &str = r#"
@group(0) @binding(0) var<storage, read_write> output: array<u32>;
@group(0) @binding(1) var<storage, read> page_table: array<u32>;
@group(0) @binding(2) var<storage, read> page0: array<u32>;
@group(0) @binding(3) var<storage, read> page1: array<u32>;
@group(0) @binding(4) var<storage, read> page2: array<u32>;
@group(0) @binding(5) var<storage, read> page3: array<u32>;
struct Layer {
  width: u32, height: u32, depth: u32, channel_count: u32,
  origin_x: u32, origin_y: u32, origin_z: u32, channel_offset: u32,
  scale_x: f32, scale_y: f32, scale_z: f32,
  translation_x: f32, translation_y: f32, translation_z: f32,
  _pad0: u32, _pad1: u32,
}
struct Channel {
  page_offset: u32, page_count: u32, window_start: f32, window_end: f32,
  opacity: f32, red: f32, green: f32, blue: f32,
}
struct Scene {
  frame_width: u32, frame_height: u32, layer_count: u32, ray_step: f32,
  annotation_count: u32, _pad0: u32, _pad1: u32, _pad2: u32,
  layers: array<Layer, 4>, channels: array<Channel, 16>,
}
@group(0) @binding(6) var<uniform> scene: Scene;
@group(0) @binding(7) var<storage, read_write> ray_distance: array<f32>;
@group(0) @binding(8) var<storage, read> rays: array<u32>;
fn sample(location: u32) -> u32 {
  let page = location >> 20u; let offset = location & 0x000fffffu;
  if (page == 0u) { return page0[offset]; }
  if (page == 1u) { return page1[offset]; }
  if (page == 2u) { return page2[offset]; }
  return page3[offset];
}
fn srgb_to_linear(value: f32) -> f32 { if (value <= 0.04045) { return value / 12.92; } return pow((value + 0.055) / 1.055, 2.4); }
fn cross2(a: vec2f, b: vec2f, point: vec2f) -> f32 { return (point.x - a.x) * (b.y - a.y) - (point.y - a.y) * (b.x - a.x); }
struct AnnotationHit { covered: bool, ray_distance: f32, colour: vec3f }
fn annotation_hit(base: u32, pixel: vec2f) -> AnnotationHit {
  let kind = rays[base]; let packed_colour = rays[base + 2u];
  let encoded = vec3f(f32((packed_colour >> 16u) & 255u) / 255.0, f32((packed_colour >> 8u) & 255u) / 255.0, f32(packed_colour & 255u) / 255.0);
  let colour = vec3f(srgb_to_linear(encoded.r), srgb_to_linear(encoded.g), srgb_to_linear(encoded.b));
  let radius = bitcast<f32>(rays[base + 3u]); let a = vec2f(bitcast<f32>(rays[base + 4u]), bitcast<f32>(rays[base + 5u])); let ad = bitcast<f32>(rays[base + 6u]);
  if (kind == 1u) { return AnnotationHit(distance(pixel, a) <= radius, ad, colour); }
  let b = vec2f(bitcast<f32>(rays[base + 7u]), bitcast<f32>(rays[base + 8u])); let bd = bitcast<f32>(rays[base + 9u]);
  if (kind == 2u) { let ab = b - a; let d = dot(ab, ab); if (d == 0.0) { return AnnotationHit(distance(pixel, a) <= radius, ad, colour); } let t = clamp(dot(pixel-a, ab)/d, 0.0, 1.0); return AnnotationHit(distance(pixel, a+t*ab) <= radius, mix(ad,bd,t),colour); }
  if (kind == 3u) { let c = vec2f(bitcast<f32>(rays[base+10u]),bitcast<f32>(rays[base+11u])); let cd=bitcast<f32>(rays[base+12u]); let ab=cross2(a,b,pixel); let bc=cross2(b,c,pixel); let ca=cross2(c,a,pixel); let covered=(ab>=0.0&&bc>=0.0&&ca>=0.0)||(ab<=0.0&&bc<=0.0&&ca<=0.0); let denominator=cross2(a,b,c); if(abs(denominator)<=0.000001){return AnnotationHit(false,0.0,colour);} let bw=cross2(a,pixel,c)/denominator; let cw=cross2(a,b,pixel)/denominator; return AnnotationHit(covered,ad+bw*(bd-ad)+cw*(cd-ad),colour); }
  return AnnotationHit(false, 0.0, colour);
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3u) {
  if (gid.x >= scene.frame_width || gid.y >= scene.frame_height) { return; }
  let pixel = gid.x + scene.frame_width * gid.y;
  let ray_base = scene.annotation_count * 13u + pixel * 8u;
  let ray_origin = vec3f(bitcast<f32>(rays[ray_base]), bitcast<f32>(rays[ray_base + 1u]), bitcast<f32>(rays[ray_base + 2u]));
  let ray_direction = vec3f(bitcast<f32>(rays[ray_base + 3u]), bitcast<f32>(rays[ray_base + 4u]), bitcast<f32>(rays[ray_base + 5u]));
  let ray_start = bitcast<f32>(rays[ray_base + 6u]);
  let ray_end = bitcast<f32>(rays[ray_base + 7u]);
  var colour = vec3f(0.0); var opacity = 0.0;
  ray_distance[pixel] = bitcast<f32>(0x7f800000u);
  if (ray_end > ray_start) {
    let steps = min(4096u, u32(ceil((ray_end - ray_start) / scene.ray_step)));
    for (var step = 0u; step < steps; step = step + 1u) {
      let travelled = ray_start + (f32(step) + 0.5) * scene.ray_step;
      if (travelled >= ray_end) { break; }
      let world = ray_origin + ray_direction * travelled;
      var sample_colour = vec3f(0.0); var sample_opacity = 0.0; var table_offset = 0u;
      for (var layer_index = 0u; layer_index < scene.layer_count; layer_index = layer_index + 1u) {
        let layer = scene.layers[layer_index];
        let voxel = (world - vec3f(layer.translation_x, layer.translation_y, layer.translation_z)) /
          vec3f(layer.scale_x, layer.scale_y, layer.scale_z) - vec3f(f32(layer.origin_x), f32(layer.origin_y), f32(layer.origin_z));
        var layer_colour = vec3f(0.0); var layer_opacity = 0.0;
        // Voxel-centred: nearest voxel, inside [-0.5, dimension - 0.5).
        if (all(voxel >= vec3f(-0.5)) && voxel.x < f32(layer.width) - 0.5 && voxel.y < f32(layer.height) - 0.5 && voxel.z < f32(layer.depth) - 0.5) {
          let coordinate = vec3u(floor(voxel + vec3f(0.5)));
          let voxel_index = coordinate.x + layer.width * (coordinate.y + layer.height * coordinate.z);
          for (var channel_index = 0u; channel_index < layer.channel_count; channel_index = channel_index + 1u) {
            let channel = scene.channels[layer.channel_offset + channel_index];
            let value = f32(sample(page_table[table_offset + channel_index * (layer.width * layer.height * layer.depth) + voxel_index]));
            let intensity = select(clamp((value - channel.window_start) / (channel.window_end - channel.window_start), 0.0, 1.0), select(0.0, 1.0, value >= channel.window_end), channel.window_start == channel.window_end);
            let alpha = intensity * channel.opacity;
            layer_colour = layer_colour + vec3f(channel.red, channel.green, channel.blue) * alpha;
            layer_opacity = min(1.0, layer_opacity + alpha);
          }
        }
        sample_colour = layer_colour + (1.0 - layer_opacity) * sample_colour;
        sample_opacity = layer_opacity + (1.0 - layer_opacity) * sample_opacity;
        table_offset = table_offset + layer.channel_count * (layer.width * layer.height * layer.depth);
      }
      let alpha = sample_opacity * 0.06;
      colour = colour + (1.0 - opacity) * sample_colour * 0.06;
      opacity = opacity + (1.0 - opacity) * alpha;
      if (opacity >= 0.01 && ray_distance[pixel] == bitcast<f32>(0x7f800000u)) { ray_distance[pixel] = travelled; }
    }
  }
  for (var primitive = 0u; primitive < scene.annotation_count; primitive = primitive + 1u) {
    let hit = annotation_hit(primitive * 13u, vec2f(f32(gid.x), f32(gid.y)));
    if (hit.covered && hit.ray_distance <= ray_distance[pixel]) { colour = hit.colour; opacity = 1.0; }
  }
  output[pixel * 2u] = pack2x16float(colour.rg);
  output[pixel * 2u + 1u] = pack2x16float(vec2f(colour.b, opacity));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_local_frame_command_and_encodes_pgm() {
        let arguments = parse_arguments(
            [
                "--output",
                "frame.pgm",
                "--color-output",
                "frame.ppm",
                "--axis",
                "x",
                "--depth-output",
                "depth.pfm",
                "--timing-output",
                "timing.json",
                "--zarr",
                "data.zarr",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(arguments.level, "0");
        assert_eq!(arguments.axis, RayAxis::X);
        assert_eq!(arguments.color_output, Some(PathBuf::from("frame.ppm")));
        assert_eq!(arguments.depth_output, Some(PathBuf::from("depth.pfm")));
        assert_eq!(arguments.timing_output, Some(PathBuf::from("timing.json")));
        assert_eq!(arguments.warm_iterations, 0);
        assert!(!arguments.annotation_fixture);
        let annotation_fixture = parse_arguments(
            [
                "--zarr",
                "data.zarr",
                "--output",
                "annotations.pgm",
                "--annotation-fixture",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert!(annotation_fixture.annotation_fixture);
        assert!(parse_arguments(
            [
                "--zarr",
                "data.zarr",
                "--output",
                "annotations.pgm",
                "--annotation-fixture",
                "--annotation-fixture",
            ]
            .map(str::to_owned)
        )
        .is_err());
        let warmed = parse_arguments(
            [
                "--zarr",
                "data.zarr",
                "--output",
                "warmed.pgm",
                "--warm-iterations",
                "3",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(warmed.warm_iterations, 3);
        assert!(parse_arguments(
            [
                "--zarr",
                "data.zarr",
                "--output",
                "bad-warm.pgm",
                "--warm-iterations",
                "0",
            ]
            .map(str::to_owned)
        )
        .is_err());
        assert_eq!(
            arguments.source,
            SourceArgument::Local(PathBuf::from("data.zarr"))
        );
        let automatic = parse_arguments(
            [
                "--zarr",
                "data.zarr",
                "--level",
                "auto",
                "--output",
                "automatic.pgm",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(automatic.level, "auto");
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let fixture_source = open_source(&SourceArgument::Local(fixture)).unwrap();
        assert_eq!(resolve_level(&fixture_source, "auto").unwrap(), "0");
        assert_eq!(resolve_level(&fixture_source, "1").unwrap(), "1");
        assert_eq!(
            physical_ray_step(&fixture_source, "0", RayAxis::Z).unwrap(),
            0.29
        );
        assert_eq!(
            physical_ray_step(&fixture_source, "0", RayAxis::Y).unwrap(),
            0.26
        );
        assert_eq!(
            physical_ray_step(&fixture_source, "0", RayAxis::X).unwrap(),
            0.26
        );
        let s3 = parse_arguments(
            [
                "--s3-bucket",
                "microscopy-data",
                "--s3-prefix",
                "study/cells3d",
                "--s3-profile",
                "newvolim-readonly",
                "--s3-region",
                "eu-north-1",
                "--output",
                "s3.pgm",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            s3.source,
            SourceArgument::S3 {
                bucket: "microscopy-data".into(),
                prefix: "study/cells3d".into(),
                profile: "newvolim-readonly".into(),
                region: Some("eu-north-1".into()),
            }
        );
        assert!(parse_arguments(
            ["--s3-bucket", "microscopy-data", "--output", "s3.pgm"].map(str::to_owned)
        )
        .is_err());
        let remote = parse_arguments(
            [
                "--https-root",
                "https://example.org/store/",
                "--allow-host",
                "example.org",
                "--output",
                "remote.pgm",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(
            remote.source,
            SourceArgument::Https {
                root: "https://example.org/store/".into(),
                allowed_hosts: vec!["example.org".into()],
            }
        );
        assert!(open_https_source("https://example.org/store/", &["example.org".into()]).is_ok());
        assert!(
            open_https_source("https://other.example/store/", &["example.org".into()]).is_err()
        );
        assert!(parse_arguments(
            [
                "--https-root",
                "https://example.org/store/",
                "--output",
                "remote.pgm"
            ]
            .map(str::to_owned)
        )
        .is_err());
        assert!(parse_arguments(["--zarr", "data.zarr"].map(str::to_owned)).is_err());
        assert_eq!(RayAxis::Y.output_dimensions([128, 64, 32]), [128, 32]);
        assert_eq!(pack_page_location(PAGE_WORDS - 1), (1 << OFFSET_BITS) - 1);
        assert_eq!(pack_page_location(PAGE_WORDS), 1 << OFFSET_BITS);
        assert_eq!(
            pack_page_location(PAGE_WORDS * 3 + 12),
            (3 << OFFSET_BITS) | 12
        );
        assert_eq!(
            encode_pgm(2, 1, &[1, 2]).unwrap(),
            b"P5\n2 1\n255\n\x01\x02"
        );
        assert_eq!(
            encode_ppm(1, 1, &[[1, 2, 3, 4]]).unwrap(),
            b"P6\n1 1\n255\n\x01\x02\x03"
        );
        assert_eq!(
            encode_pfm(1, 1, &[f32::INFINITY]).unwrap(),
            [b"Pf\n1 1\n-1.0\n".as_slice(), &f32::INFINITY.to_le_bytes()].concat()
        );
        let depth = encode_pfm(2, 2, &[1.0, 2.0, 3.0, f32::INFINITY]).unwrap();
        let header = b"Pf\n2 2\n-1.0\n";
        assert_eq!(&depth[..header.len()], header);
        let (value_bytes, remainder) = depth[header.len()..].as_chunks::<4>();
        assert!(remainder.is_empty());
        let values = value_bytes
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect::<Vec<_>>();
        assert_eq!(values, vec![3.0, f32::INFINITY, 1.0, 2.0]);
        assert_eq!(parse_srgb_hex_bytes("FF3355"), Some([255, 51, 85]));
        assert_eq!(parse_srgb_hex_bytes("bad"), None);
        assert_eq!(fixture_annotation_words(128, 64).len(), 26);
        assert_eq!(unpack_f16_pair(0x3c00_0000), [0.0, 1.0]);
        assert_eq!(unpack_f16_pair(0x3c00_3c00), [1.0, 1.0]);
        assert!(half_to_f32(0x7c00).is_infinite());
        assert_eq!(linear_to_srgb_byte(0.0), 0);
        assert_eq!(linear_to_srgb_byte(1.0), 255);
        assert_eq!(
            RenderTiming {
                adapter_device_ms: 2.5,
                setup_upload_ms: 3.25,
                dispatch_readback_ms: 4.0,
                warm_iterations: 3,
                warm_dispatch_ms: 6.0,
            }
            .as_json(1.5, 128, 64),
            concat!(
                "{\"kind\":\"newvolim-native-wgpu-frame\",",
                "\"width\":128,\"height\":64,\"loadMs\":1.500000,",
                "\"adapterDeviceMs\":2.500000,\"setupUploadMs\":3.250000,",
                "\"dispatchReadbackMs\":4.000000,\"warmIterations\":3,",
                "\"warmDispatchMs\":6.000000}\n"
            )
        );
        let transfer = transfer_function(&fixture_source).unwrap();
        assert_eq!(transfer.window, [0.0, 65535.0]);
        assert_eq!(transfer.color_srgb, [255, 51, 85]);
        assert_eq!(transfer.color_linear, [1.0, 0.033_104_762, 0.090_841_73]);
    }

    #[test]
    fn multi_page_direct_volume_is_flattened_at_the_fixed_page_boundary() {
        let transfer = TransferFunction {
            color_linear: [1.0, 1.0, 1.0],
            color_srgb: [255, 255, 255],
            window: [0.0, 3.0],
            opacity: 1.0,
        };
        let submission = PortablePageSubmission::from_uploads([
            PortablePageUpload {
                page: 0,
                words: vec![1; PAGE_WORDS],
            },
            PortablePageUpload {
                page: 1,
                words: vec![3],
            },
        ])
        .unwrap();
        let frame = NativePortableFrameInput::new(
            vec![NativeLayerDescriptor {
                layer_id: LayerId(0),
                page_offset: 0,
                page_count: 2,
                transform: LayerTransform::IDENTITY,
            }],
            submission,
        )
        .unwrap();
        let volume = NativePortableVolumeInput::new(
            frame,
            [PAGE_WORDS as u32 + 1, 1, 1],
            PortableScalarType::Uint16,
            PortableChannelTransfer {
                color_srgb: transfer.color_srgb,
                window_start: 0.0,
                window_end: 3.0,
                opacity: 1.0,
            },
        )
        .unwrap();
        let channels = portable_volume_u16_channels(&volume).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].voxels.len(), PAGE_WORDS + 1);
        assert_eq!(channels[0].voxels[PAGE_WORDS - 1], 1);
        assert_eq!(channels[0].voxels[PAGE_WORDS], 3);
    }

    #[test]
    fn scene_layer_records_preserve_ordered_absolute_page_and_transform_data() {
        let transfer = PortableChannelTransfer {
            color_srgb: [255, 0, 0],
            window_start: 0.0,
            window_end: 10.0,
            opacity: 0.5,
        };
        let first_transform = LayerTransform::IDENTITY;
        let second_transform = LayerTransform::new([2.0, 3.0, 4.0], [5.0, 6.0, 7.0]).unwrap();
        let mut scene = NativePortableSceneInput::new(
            NativePortableFrameInput::new(
                vec![
                    NativeLayerDescriptor {
                        layer_id: LayerId(3),
                        page_offset: 0,
                        page_count: 1,
                        transform: first_transform,
                    },
                    NativeLayerDescriptor {
                        layer_id: LayerId(4),
                        page_offset: 1,
                        page_count: 1,
                        transform: second_transform,
                    },
                ],
                PortablePageSubmission::from_uploads([
                    PortablePageUpload {
                        page: 0,
                        words: vec![1; 8],
                    },
                    PortablePageUpload {
                        page: 1,
                        words: vec![2; 8],
                    },
                ])
                .unwrap(),
            )
            .unwrap(),
            vec![
                newvolim_render::PortableSceneLayerInput {
                    layer_id: LayerId(3),
                    transform: first_transform,
                    voxel_origin_xyz: [0, 0, 0],
                    dimensions_xyz: [2, 2, 2],
                    scalar_type: PortableScalarType::Uint16,
                    channels: vec![newvolim_render::PortableVolumeChannel {
                        page_offset: 0,
                        page_count: 1,
                        transfer,
                    }],
                },
                newvolim_render::PortableSceneLayerInput {
                    layer_id: LayerId(4),
                    transform: second_transform,
                    voxel_origin_xyz: [9, 8, 7],
                    dimensions_xyz: [2, 2, 2],
                    scalar_type: PortableScalarType::Uint16,
                    channels: vec![newvolim_render::PortableVolumeChannel {
                        page_offset: 1,
                        page_count: 1,
                        transfer,
                    }],
                },
            ],
        )
        .unwrap();
        let packed = pack_portable_scene_layers(&scene).unwrap();
        let channels = pack_portable_scene_channels(&scene).unwrap();
        assert_eq!(&packed[..8], &[2, 2, 2, 1, 0, 0, 0, 0]);
        assert_eq!(&packed[16..24], &[2, 2, 2, 1, 9, 8, 7, 1]);
        assert_eq!(&channels[..2], &[0, 1]);
        assert_eq!(&channels[8..10], &[1, 1]);
        assert_eq!(channels[5], 1.0_f32.to_bits());
        assert_eq!(channels[6], 0.0_f32.to_bits());
        assert_eq!(
            packed[24..30],
            [
                2.0_f32.to_bits(),
                3.0_f32.to_bits(),
                4.0_f32.to_bits(),
                5.0_f32.to_bits(),
                6.0_f32.to_bits(),
                7.0_f32.to_bits()
            ]
        );
        let camera = newvolim_render::NativePortableSceneCameraDrawInput::new(
            newvolim_render::NativePortableSceneDrawInput::new(scene.clone(), [1, 1], Vec::new())
                .unwrap(),
            vec![
                newvolim_render::PortableWorldRay::new([0.5, 0.5, -1.0], [0.0, 0.0, 1.0]).unwrap(),
            ],
        )
        .unwrap();
        // Voxel-centred box: the first layer's two z voxels span local [-0.5, 1.5], entered by
        // a ray starting at z = -1 after 0.5 and left after 2.5.
        assert_eq!(
            portable_scene_ray_ranges(&camera).unwrap(),
            vec![[0.5, 2.5]]
        );
        let gpu_packet = prepare_portable_scene_gpu_packet(&camera).unwrap();
        assert_eq!(&gpu_packet.config[..5], &[1, 1, 2, 0.5_f32.to_bits(), 0]);
        assert_eq!(gpu_packet.page_table.len(), 16);
        assert_eq!(gpu_packet.page_table[0], 0);
        assert_eq!(gpu_packet.page_table[8], 1 << OFFSET_BITS);
        assert_eq!(gpu_packet.rays.len(), 8);
        assert_eq!(f32::from_bits(gpu_packet.rays[6]), 0.5);
        assert_eq!(f32::from_bits(gpu_packet.rays[7]), 2.5);
        scene.layers[1].voxel_origin_xyz = [u64::from(u32::MAX) + 1, 0, 0];
        assert!(pack_portable_scene_layers(&scene).is_err());
    }

    /// The typed packet test above is adapter-free; this opt-in check additionally asks the
    /// local backend to validate the scene shader's resource layout and bounded world-ray loop.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn scene_shader_compiles_on_a_local_adapter() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .unwrap();
        let (device, _) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .unwrap();
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("newvolim scene shader validation"),
            source: wgpu::ShaderSource::Wgsl(SCENE_SHADER.into()),
        });
        let _pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("newvolim scene shader validation"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        assert!(pollster::block_on(error_scope.pop()).is_none());
    }

    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn scene_camera_draw_executes_a_world_ray_through_a_static_page() {
        let transfer = PortableChannelTransfer {
            color_srgb: [255, 0, 0],
            window_start: 0.0,
            window_end: 1.0,
            opacity: 1.0,
        };
        let scene = NativePortableSceneInput::new(
            NativePortableFrameInput::new(
                vec![NativeLayerDescriptor {
                    layer_id: LayerId(0),
                    page_offset: 0,
                    page_count: 1,
                    transform: LayerTransform::IDENTITY,
                }],
                PortablePageSubmission::from_uploads([PortablePageUpload {
                    page: 0,
                    words: vec![1],
                }])
                .unwrap(),
            )
            .unwrap(),
            vec![newvolim_render::PortableSceneLayerInput {
                layer_id: LayerId(0),
                transform: LayerTransform::IDENTITY,
                voxel_origin_xyz: [0, 0, 0],
                dimensions_xyz: [1, 1, 1],
                scalar_type: PortableScalarType::Uint16,
                channels: vec![newvolim_render::PortableVolumeChannel {
                    page_offset: 0,
                    page_count: 1,
                    transfer,
                }],
            }],
        )
        .unwrap();
        let front_annotation = PortableAnnotationPrimitive {
            kind: PortableAnnotationPrimitiveKind::Point,
            annotation_id: 1,
            color_srgb: [0, 255, 0],
            radius: 1.0,
            vertices: [ProjectedAnnotationVertex {
                pixel: [0.0, 0.0],
                ray_distance: 0.0,
            }; 3],
        };
        let behind_annotation = PortableAnnotationPrimitive {
            kind: PortableAnnotationPrimitiveKind::Point,
            annotation_id: 2,
            color_srgb: [0, 0, 255],
            radius: 1.0,
            vertices: [ProjectedAnnotationVertex {
                pixel: [0.0, 0.0],
                ray_distance: f32::MAX,
            }; 3],
        };
        let annotation_words = front_annotation
            .words()
            .into_iter()
            .chain(behind_annotation.words())
            .collect();
        let camera = newvolim_render::NativePortableSceneCameraDrawInput::new(
            newvolim_render::NativePortableSceneDrawInput::new(scene, [1, 1], annotation_words)
                .unwrap(),
            vec![
                // Through the single voxel's position: layer boxes are voxel-centred, so the voxel
                // spans [-0.5, 0.5] and (0.5, 0.5) would sit on its edge.
                newvolim_render::PortableWorldRay::new([0.0, 0.0, -1.0], [0.0, 0.0, 1.0]).unwrap(),
            ],
        )
        .unwrap();
        let rendered = render_portable_scene_camera_draw(&camera, 0).unwrap();
        assert_eq!(rendered.rgba.len(), 1);
        assert!(
            rendered.rgba[0][1] > 0
                && rendered.rgba[0][0] == 0
                && rendered.rgba[0][2] == 0
                && rendered.rgba[0][3] > 0
        );
        assert!(rendered.ray_distances[0].is_finite());
    }

    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn scene_camera_draw_composites_later_overlapping_layer_over_earlier_layer() {
        let transfer = |color_srgb| PortableChannelTransfer {
            color_srgb,
            window_start: 0.0,
            window_end: 1.0,
            opacity: 1.0,
        };
        let scene = NativePortableSceneInput::new(
            NativePortableFrameInput::new(
                vec![
                    NativeLayerDescriptor {
                        layer_id: LayerId(0),
                        page_offset: 0,
                        page_count: 1,
                        transform: LayerTransform::IDENTITY,
                    },
                    NativeLayerDescriptor {
                        layer_id: LayerId(1),
                        page_offset: 1,
                        page_count: 1,
                        transform: LayerTransform::IDENTITY,
                    },
                ],
                PortablePageSubmission::from_uploads([
                    PortablePageUpload {
                        page: 0,
                        words: vec![1],
                    },
                    PortablePageUpload {
                        page: 1,
                        words: vec![1],
                    },
                ])
                .unwrap(),
            )
            .unwrap(),
            vec![
                newvolim_render::PortableSceneLayerInput {
                    layer_id: LayerId(0),
                    transform: LayerTransform::IDENTITY,
                    voxel_origin_xyz: [0, 0, 0],
                    dimensions_xyz: [1, 1, 1],
                    scalar_type: PortableScalarType::Uint16,
                    channels: vec![newvolim_render::PortableVolumeChannel {
                        page_offset: 0,
                        page_count: 1,
                        transfer: transfer([255, 0, 0]),
                    }],
                },
                newvolim_render::PortableSceneLayerInput {
                    layer_id: LayerId(1),
                    transform: LayerTransform::IDENTITY,
                    voxel_origin_xyz: [0, 0, 0],
                    dimensions_xyz: [1, 1, 1],
                    scalar_type: PortableScalarType::Uint16,
                    channels: vec![newvolim_render::PortableVolumeChannel {
                        page_offset: 1,
                        page_count: 1,
                        transfer: transfer([0, 255, 0]),
                    }],
                },
            ],
        )
        .unwrap();
        let camera = newvolim_render::NativePortableSceneCameraDrawInput::new(
            newvolim_render::NativePortableSceneDrawInput::new(scene, [1, 1], Vec::new()).unwrap(),
            vec![
                // Through the single voxel's position: layer boxes are voxel-centred, so the voxel
                // spans [-0.5, 0.5] and (0.5, 0.5) would sit on its edge.
                newvolim_render::PortableWorldRay::new([0.0, 0.0, -1.0], [0.0, 0.0, 1.0]).unwrap(),
            ],
        )
        .unwrap();
        let pixel = render_portable_scene_camera_draw(&camera, 0).unwrap().rgba[0];
        assert!(pixel[1] > 0 && pixel[0] == 0 && pixel[3] > 0);
    }

    /// Requires a local adapter because it validates the compiled WGPU channel loop, not merely
    /// the typed packet boundary. Two opaque channels must contribute linear red and green to
    /// the same ray sample instead of one static page shadowing the other.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn direct_portable_draw_composites_ordered_channel_ranges() {
        let transfer = |color_srgb| PortableChannelTransfer {
            color_srgb,
            window_start: 0.0,
            window_end: 1.0,
            opacity: 1.0,
        };
        let pages = PortablePageSubmission::from_uploads([
            PortablePageUpload {
                page: 0,
                words: vec![1; 64],
            },
            PortablePageUpload {
                page: 1,
                words: vec![1; 64],
            },
        ])
        .unwrap();
        let frame = NativePortableFrameInput::new(
            vec![NativeLayerDescriptor {
                layer_id: LayerId(0),
                page_offset: 0,
                page_count: 2,
                transform: LayerTransform::IDENTITY,
            }],
            pages,
        )
        .unwrap();
        let volume = NativePortableVolumeInput::new_channels(
            frame,
            [4, 4, 4],
            PortableScalarType::Uint16,
            vec![
                newvolim_render::PortableVolumeChannel {
                    page_offset: 0,
                    page_count: 1,
                    transfer: transfer([255, 0, 0]),
                },
                newvolim_render::PortableVolumeChannel {
                    page_offset: 1,
                    page_count: 1,
                    transfer: transfer([0, 255, 0]),
                },
            ],
        )
        .unwrap();
        let draw = NativePortableDrawInput::new(
            volume,
            [4, 4],
            PortableCameraControls::new([0, 0], 1.0).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let rendered = render_portable_draw(&draw, RayAxis::Z, 1.0, 0).unwrap();
        assert!(rendered
            .rgba
            .iter()
            .all(|pixel| pixel[0] > 0 && pixel[1] > 0));
        assert!(rendered
            .rgba
            .iter()
            .all(|pixel| pixel[2] == 0 && pixel[3] > 0));
    }

    /// Requires a real local WGPU adapter, so ordinary portable unit suites leave it opt-in.
    /// It specifically covers the non-default camera branch: every pixel gets an independent
    /// ray rather than falling back to the legacy axis recorder.
    #[test]
    #[ignore = "requires a local WGPU adapter"]
    fn camera_packet_marches_nondefault_per_pixel_rays() {
        let transfer = TransferFunction {
            color_linear: [1.0, 0.0, 0.0],
            color_srgb: [255, 0, 0],
            window: [0.0, 1.0],
            opacity: 1.0,
        };
        let volume = direct_volume_input(&[1; 8], [2, 2, 2], transfer).unwrap();
        let draw = NativePortableDrawInput::new(
            volume,
            [2, 2],
            PortableCameraControls::new([13, -7], 1.2).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let rays = [
            [0.5, 0.5, -1.0],
            [1.5, 0.5, -1.0],
            [0.5, 1.5, -1.0],
            [1.5, 1.5, -1.0],
        ]
        .into_iter()
        .map(|origin_xyz| newvolim_render::PortableCameraRay {
            origin_xyz,
            direction_xyz: [0.0, 0.0, 1.0],
        })
        .collect();
        let input = NativePortableCameraDrawInput::new(draw, rays).unwrap();
        let frame = render_portable_camera_draw(&input, 0).unwrap();
        assert_eq!(frame.rgba.len(), 4);
        assert!(frame.rgba.iter().all(|pixel| pixel[3] > 0));
        assert!(frame
            .ray_distances
            .iter()
            .all(|distance| distance.is_finite()));
    }
}
