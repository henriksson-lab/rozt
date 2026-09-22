//! The demand-driven residency loop, as a client.
//!
//! The server's demand route (`newvolim-portable`) runs one `PortableResidencyLoop` per
//! (layer, channel), dispatches the scene shader, reads the chunks it missed, plans them into
//! the four static pages, reads them from disk and dispatches again until nothing is missing.
//! This crate is that loop with the two host-bound steps taken out: it does not read chunks
//! (it says which it needs, by layer, level, channel and chunk index) and it does not dispatch
//! (it hands out the byte-identical [`SceneDvrDispatch`] the recorder would build). A browser
//! page drives it with HTTP fetches and WebGPU; a native test drives it with the session's own
//! reads and the local adapter and pins the frame against the server's.
//!
//! What persists across frames is the [`ChunkCache`]: chunk words keyed by channel ordinal,
//! level and chunk index, bounded in words, so a camera move at the same level fetches only
//! what came into view.

use std::collections::{HashMap, VecDeque};

use palace_core::gpu::{
    scene_dvr_dispatch, PortableChunkGrid, PortableDvrPageFrameInput, PortableDvrSceneChannel,
    PortableDvrSceneFrameInput, PortableDvrSceneLayer, PortableDvrSceneResidency,
    PortableDvrVolumeLevel, PortableFeedbackKey, PortablePageTable, PortableRayInterval,
    PortableResidencyLoop, PortableResidencyStep, PortableResidencyTag, PortableTensorPage,
    PortableTransferFunction, SceneDvrDispatch,
};
use palace_core::{data::Vector, dim::D3};
use serde::{Deserialize, Serialize};

/// The residency map the demand route builds: 4096 slots, 16 probes.
pub const PAGE_TABLE_CAPACITY: usize = 4_096;
pub const PAGE_TABLE_PROBES: usize = 16;
/// The demand route's bound on feedback iterations per frame.
pub const MAX_ITERATIONS: usize = 24;
/// A ray on the wire: origin, direction, near, far as `f32` bits.
pub const RAY_WORDS: usize = 8;

/// Everything about a frame except the pages and the rays: the server's `prepare_demand_scene`
/// minus what the client fetches on demand. Small enough to be JSON.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenePlan {
    pub width: u32,
    pub height: u32,
    /// Fitted camera and scene box, enough to produce the exact ray table locally.
    pub camera: RayCamera,
    /// The pyramid level of each visible image layer, in plan order.
    pub levels: Vec<u32>,
    /// How many levels each layer has, so the client can coarsen when a level's working set
    /// exceeds the pages.
    pub level_counts: Vec<usize>,
    pub step: f32,
    pub opacity_reference: f32,
    pub layers: Vec<PlanLayer>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RayCamera {
    /// Palace's fitted camera uses the source array's ZYX axis order.
    pub origin_zyx: [f32; 3],
    pub forward_zyx: [f32; 3],
    pub right_zyx: [f32; 3],
    pub up_zyx: [f32; 3],
    pub focal_scale: f32,
    /// NGFF translation is f64; addition to the f32 fitted origin happens before rounding.
    pub translation_xyz: [f64; 3],
    pub minimum: [f32; 3],
    pub maximum: [f32; 3],
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanLayer {
    pub layer_id: u64,
    pub level: u32,
    pub dimensions_xyz: [u32; 3],
    pub chunk_shape_xyz: [u32; 3],
    pub minimum: [f32; 3],
    pub maximum: [f32; 3],
    pub channels: Vec<PlanChannel>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanChannel {
    /// The channel's index in the source array.
    pub source_index: u32,
    /// The scene-wide channel ordinal, which the residency tag and the page owners derive from.
    pub ordinal: u32,
    pub tag: u32,
    pub owner_base: u64,
    pub transfer_min: f32,
    pub transfer_max: f32,
    pub transfer_entries: Vec<[u8; 4]>,
}

/// One chunk the client must fetch: enough for the server to read it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRequest {
    pub layer_id: u64,
    pub level: u32,
    pub source_index: u32,
    pub ordinal: u32,
    pub chunk_index: u32,
    /// The chunk's grid coordinate, which is what the server's chunk planner takes.
    pub chunk_xyz: [u32; 3],
}

/// What one absorption of the shader's request buffer decided.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    /// No channel missed anything: the last frame is final.
    Complete,
    /// At least one channel planned more chunks; fetch what [`ClientResidency::missing_chunks`]
    /// lists and dispatch again.
    Planned,
    /// A channel's working set does not fit the four pages at this level; replan coarser.
    ExceedsPortableBound { required_pages: usize },
}

/// Chunk words that survive across frames, bounded in words, oldest out first.
#[derive(Clone, Debug, Default)]
pub struct ChunkCache {
    words: HashMap<(u32, u32, u32), Vec<u32>>,
    order: VecDeque<(u32, u32, u32)>,
    total_words: usize,
    budget_words: usize,
}

impl ChunkCache {
    pub fn new(budget_words: usize) -> Self {
        Self {
            budget_words,
            ..Self::default()
        }
    }

    fn key(request: &ChunkRequest) -> (u32, u32, u32) {
        (request.ordinal, request.level, request.chunk_index)
    }

    pub fn contains(&self, request: &ChunkRequest) -> bool {
        self.words.contains_key(&Self::key(request))
    }

    pub fn get(&self, request: &ChunkRequest) -> Option<&[u32]> {
        self.words.get(&Self::key(request)).map(Vec::as_slice)
    }

    /// Insert one chunk, evicting the oldest until the budget holds (the new chunk always fits,
    /// even alone over budget, so a frame can never starve).
    pub fn insert(&mut self, request: &ChunkRequest, words: Vec<u32>) {
        let key = Self::key(request);
        if let Some(previous) = self.words.insert(key, words) {
            self.total_words -= previous.len();
            self.order.retain(|k| *k != key);
        }
        self.total_words += self.words[&key].len();
        self.order.push_back(key);
        while self.total_words > self.budget_words && self.order.len() > 1 {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.words.remove(&oldest) {
                self.total_words -= evicted.len();
            }
        }
    }

    pub fn total_words(&self) -> usize {
        self.total_words
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

struct ChannelLoop {
    layer: usize,
    channel: usize,
    residency: PortableResidencyLoop,
}

/// One frame's residency: the plan, its rays, one loop per channel, and the cache.
pub struct ClientResidency {
    plan: ScenePlan,
    rays: Vec<PortableRayInterval>,
    loops: Vec<ChannelLoop>,
    cache: ChunkCache,
}

impl ClientResidency {
    /// Start a frame from a plan and its rays (as the wire words), over a cache from earlier
    /// frames. Every loop starts empty, exactly as the server's do.
    pub fn new(plan: ScenePlan, ray_words: &[u32], cache: ChunkCache) -> Result<Self, String> {
        let rays = rays_from_words(ray_words)?;
        let pixels = plan.width as usize * plan.height as usize;
        if rays.len() != pixels {
            return Err(format!(
                "plan is {}×{} but {} rays were given",
                plan.width,
                plan.height,
                rays.len()
            ));
        }
        if plan.levels.len() != plan.layers.len() || plan.level_counts.len() != plan.layers.len() {
            return Err("plan levels do not match its layers".into());
        }
        let mut loops = Vec::new();
        for (layer_index, layer) in plan.layers.iter().enumerate() {
            let grid = PortableChunkGrid::new(layer.dimensions_xyz, layer.chunk_shape_xyz)
                .ok_or_else(|| format!("layer {} has an empty extent or chunk", layer.layer_id))?;
            for (channel_index, channel) in layer.channels.iter().enumerate() {
                if PortableResidencyTag::compose(channel.ordinal, layer.level) != Some(channel.tag)
                {
                    return Err(format!(
                        "channel ordinal {} at level {} does not give tag {}",
                        channel.ordinal, layer.level, channel.tag
                    ));
                }
                let residency = PortableResidencyLoop::new(
                    channel.tag,
                    grid,
                    channel.owner_base,
                    PAGE_TABLE_CAPACITY,
                    PAGE_TABLE_PROBES,
                    MAX_ITERATIONS,
                )
                .ok_or_else(|| "residency loop could not be started".to_owned())?;
                loops.push(ChannelLoop {
                    layer: layer_index,
                    channel: channel_index,
                    residency,
                });
            }
        }
        if loops.is_empty() {
            return Err("plan has no channels".into());
        }
        Ok(Self {
            plan,
            rays,
            loops,
            cache,
        })
    }

    pub fn plan(&self) -> &ScenePlan {
        &self.plan
    }

    pub fn cache(&self) -> &ChunkCache {
        &self.cache
    }

    pub fn into_cache(self) -> ChunkCache {
        self.cache
    }

    fn request_for(&self, channel_loop: &ChannelLoop, chunk_index: u32) -> ChunkRequest {
        let layer = &self.plan.layers[channel_loop.layer];
        let channel = &layer.channels[channel_loop.channel];
        let chunk_xyz = PortableChunkGrid::new(layer.dimensions_xyz, layer.chunk_shape_xyz)
            .and_then(|grid| grid.coordinate_of(chunk_index))
            .unwrap_or([u32::MAX; 3]);
        ChunkRequest {
            layer_id: layer.layer_id,
            level: layer.level,
            source_index: channel.source_index,
            ordinal: channel.ordinal,
            chunk_index,
            chunk_xyz,
        }
    }

    /// The planned chunks the cache does not hold yet, in plan order.
    pub fn missing_chunks(&self) -> Vec<ChunkRequest> {
        let mut missing = Vec::new();
        for channel_loop in &self.loops {
            for chunk in channel_loop.residency.plan().chunks() {
                let request = self.request_for(channel_loop, chunk.chunk_index);
                if !self.cache.contains(&request) {
                    missing.push(request);
                }
            }
        }
        missing
    }

    pub fn insert_chunk(&mut self, request: &ChunkRequest, words: Vec<u32>) {
        self.cache.insert(request, words);
    }

    /// Feed the shader's request buffer (all `request_capacity` words, `u32::MAX` for empty
    /// slots) to the loops: exactly the server's decoding and routing by channel ordinal.
    pub fn absorb_requests(&mut self, request_words: &[u32]) -> Result<StepOutcome, String> {
        let mut per_loop: Vec<Vec<PortableFeedbackKey>> = vec![Vec::new(); self.loops.len()];
        for &packed in request_words {
            if packed == u32::MAX {
                continue;
            }
            let key = PortableFeedbackKey::new(packed & 0x00ff_ffff, packed >> 24)
                .ok_or_else(|| format!("request {packed:#x} is not a feedback key"))?;
            let ordinal = PortableResidencyTag::channel(key.level());
            let slot = self
                .loops
                .iter()
                .position(|channel_loop| {
                    let layer = &self.plan.layers[channel_loop.layer];
                    layer.channels[channel_loop.channel].ordinal == ordinal
                })
                .ok_or_else(|| format!("request {packed:#x} names channel ordinal {ordinal}, which this frame has not"))?;
            per_loop[slot].push(key);
        }
        let mut complete = true;
        for (channel_loop, keys) in self.loops.iter_mut().zip(per_loop) {
            match channel_loop
                .residency
                .absorb(keys)
                .ok_or("scene shader demanded a chunk outside its own channel")?
            {
                PortableResidencyStep::Complete => {}
                PortableResidencyStep::Planned => complete = false,
                PortableResidencyStep::ExceedsPortableBound { required_pages } => {
                    return Ok(StepOutcome::ExceedsPortableBound { required_pages })
                }
                other => return Err(format!("residency loop stopped: {other:?}")),
            }
        }
        Ok(if complete {
            StepOutcome::Complete
        } else {
            StepOutcome::Planned
        })
    }

    /// The pages of one channel from the cache, in the plan's placement: the server's
    /// `demand_scene_layer_pages` + `portable_chunk_plan_pages`, with the cache for the disk.
    fn channel_pages(&self, channel_loop: &ChannelLoop) -> Result<Vec<PortableTensorPage>, String> {
        let plan = channel_loop.residency.plan();
        let layer = &self.plan.layers[channel_loop.layer];
        let channel = &layer.channels[channel_loop.channel];
        if plan.chunks().is_empty() {
            // The bootstrap frame: a one-word placeholder with this channel's own owner, so page
            // owners never alias across channels.
            return PortableTensorPage::new(channel.owner_base, vec![0])
                .map(|page| vec![page])
                .ok_or_else(|| "placeholder page is invalid".to_owned());
        }
        let pages = assemble_pages(plan, |chunk_index| {
            let request = self.request_for(channel_loop, chunk_index);
            self.cache.get(&request)
        })?;
        pages
            .into_iter()
            .zip(plan.page_owners())
            .map(|(words, &owner)| {
                PortableTensorPage::new(owner, words)
                    .ok_or_else(|| "assembled page is invalid".to_owned())
            })
            .collect()
    }

    /// The admitted scene and its residency map for the current plans: the server's
    /// `assemble_demand_scene`.
    pub fn assemble(&self) -> Result<(PortableDvrSceneFrameInput, PortablePageTable), String> {
        let mut table = PortablePageTable::new(PAGE_TABLE_CAPACITY, PAGE_TABLE_PROBES)
            .ok_or("residency map could not be built")?;
        let mut bound_pages = 0_usize;
        let mut scene_layers = Vec::with_capacity(self.plan.layers.len());
        let mut loop_index = 0_usize;
        for (layer_index, layer) in self.plan.layers.iter().enumerate() {
            let mut scene_channels = Vec::with_capacity(layer.channels.len());
            for (channel_index, channel) in layer.channels.iter().enumerate() {
                let channel_loop = &self.loops[loop_index];
                if channel_loop.layer != layer_index || channel_loop.channel != channel_index {
                    return Err("residency loops are out of plan order".into());
                }
                table
                    .insert_plan(channel_loop.residency.plan())
                    .ok_or("two channels collided in the residency map")?;
                let pages = self.channel_pages(channel_loop)?;
                bound_pages += pages.len();
                if bound_pages > PortableDvrPageFrameInput::MAX_PAGES {
                    return Err(format!(
                        "the scene needs {bound_pages} static pages, beyond the portable bound"
                    ));
                }
                let volume = PortableDvrVolumeLevel::new_demand_resident(
                    layer.dimensions_xyz,
                    layer.minimum,
                    layer.maximum,
                    pages,
                )
                .ok_or("scene volume is invalid")?;
                let transfer = PortableTransferFunction::new(
                    channel.transfer_min,
                    channel.transfer_max,
                    channel.transfer_entries.clone(),
                )
                .ok_or("transfer function is invalid")?;
                scene_channels.push(PortableDvrSceneChannel::with_residency(
                    volume,
                    transfer,
                    PortableDvrSceneResidency::new(layer.chunk_shape_xyz, channel.tag)
                        .ok_or("residency tag is outside the portable key")?,
                ));
                loop_index += 1;
            }
            scene_layers
                .push(PortableDvrSceneLayer::new(scene_channels).ok_or("scene layer is invalid")?);
        }
        let input = PortableDvrSceneFrameInput::new(
            self.plan.width,
            self.plan.height,
            scene_layers,
            self.rays.clone(),
            self.plan.step,
            self.plan.opacity_reference,
        )
        .ok_or("scene frame is not admitted")?;
        Ok((input, table))
    }

    /// The nine bindings for the scene shader, byte-identical to the recorder's.
    pub fn dispatch(&self) -> Result<SceneDvrDispatch, String> {
        let (input, table) = self.assemble()?;
        scene_dvr_dispatch(&input, Some(&table), PAGE_TABLE_CAPACITY, PAGE_TABLE_PROBES)
    }
}

/// The pages of one channel from its plan and a source of chunk words: chunks in plan order,
/// each placed at the plan's `first_word` of its page (the placement is checked, not
/// trusted), page lengths as planned. Shared by the server's demand route (words from the
/// session cache) and the browser (words from its own cache), so the two cannot pack
/// differently.
pub fn assemble_pages<'a>(
    plan: &palace_core::gpu::PortableChunkPlan,
    words_for: impl Fn(u32) -> Option<&'a [u32]>,
) -> Result<Vec<Vec<u32>>, String> {
    let mut pages: Vec<Vec<u32>> = plan
        .page_words()
        .iter()
        .map(|words| Vec::with_capacity(*words as usize))
        .collect();
    for chunk in plan.chunks() {
        let words = words_for(chunk.chunk_index)
            .ok_or_else(|| format!("chunk {} is planned but not available", chunk.chunk_index))?;
        let expected = chunk
            .logical_xyz
            .iter()
            .map(|&extent| extent as usize)
            .product::<usize>();
        if words.len() != expected {
            return Err(format!(
                "chunk {} has {} words, its logical extent needs {expected}",
                chunk.chunk_index,
                words.len()
            ));
        }
        let page = pages.get_mut(chunk.page as usize).ok_or_else(|| {
            format!(
                "chunk {} is planned into page {}, beyond the plan",
                chunk.chunk_index, chunk.page
            )
        })?;
        if page.len() != chunk.first_word as usize {
            return Err(format!(
                "chunk {} is placed at word {} but the page holds {}",
                chunk.chunk_index,
                chunk.first_word,
                page.len()
            ));
        }
        page.extend_from_slice(words);
    }
    for (page, &planned) in pages.iter().zip(plan.page_words()) {
        if page.len() != planned as usize {
            return Err(format!(
                "page holds {} words, the plan says {planned}",
                page.len()
            ));
        }
    }
    Ok(pages)
}

/// Rays as the server sends them: 8 `f32` bit patterns per pixel.
pub fn rays_from_words(words: &[u32]) -> Result<Vec<PortableRayInterval>, String> {
    if words.len() % RAY_WORDS != 0 {
        return Err(format!("{} ray words are not whole rays", words.len()));
    }
    words
        .chunks_exact(RAY_WORDS)
        .enumerate()
        .map(|(index, ray)| {
            let f = |at: usize| f32::from_bits(ray[at]);
            PortableRayInterval::new([f(0), f(1), f(2)], [f(3), f(4), f(5)], f(6), f(7))
                .ok_or_else(|| format!("ray {index} is not a unit interval"))
        })
        .collect()
}

pub fn ray_words(rays: &[PortableRayInterval]) -> Vec<u32> {
    rays.iter().flat_map(|ray| ray.words()).collect()
}

/// Expand the plan's fitted camera into the same clipped world rays as the server's demand route.
pub fn ray_words_for_plan(plan: &ScenePlan) -> Result<Vec<u32>, String> {
    let camera = &plan.camera;
    if plan.width == 0
        || plan.height == 0
        || !camera.focal_scale.is_finite()
        || camera
            .origin_zyx
            .iter()
            .chain(&camera.forward_zyx)
            .chain(&camera.right_zyx)
            .chain(&camera.up_zyx)
            .chain(&camera.minimum)
            .chain(&camera.maximum)
            .any(|value| !value.is_finite())
        || camera
            .translation_xyz
            .iter()
            .any(|value| !value.is_finite())
        || camera
            .minimum
            .iter()
            .zip(camera.maximum)
            .any(|(min, max)| *min >= max)
    {
        return Err("plan has an invalid ray camera".into());
    }
    let count = (plan.width as usize)
        .checked_mul(plan.height as usize)
        .and_then(|pixels| pixels.checked_mul(RAY_WORDS))
        .ok_or("plan ray count overflows usize")?;
    let forward: Vector<D3, f32> = camera.forward_zyx.into();
    let right: Vector<D3, f32> = camera.right_zyx.into();
    let up: Vector<D3, f32> = camera.up_zyx.into();
    let origin: [f32; 3] = std::array::from_fn(|axis| {
        (f64::from(camera.origin_zyx[2 - axis]) + camera.translation_xyz[axis]) as f32
    });
    let aspect = plan.width as f32 / plan.height as f32;
    let mut words = Vec::with_capacity(count);
    for y in 0..plan.height {
        let vertical = 1.0 - (2.0 * (y as f32 + 0.5) / plan.height as f32);
        for x in 0..plan.width {
            let horizontal = (2.0 * (x as f32 + 0.5) / plan.width as f32) - 1.0;
            let direction = (forward
                + right.scale(horizontal * aspect * camera.focal_scale)
                + up.scale(vertical * camera.focal_scale))
            .normalized();
            let direction_xyz = [direction[2], direction[1], direction[0]];
            let ray = PortableRayInterval::new(origin, direction_xyz, 0.0, f32::MAX)
                .ok_or("plan generated an invalid ray")?;
            let ray = ray
                .clipped_to_aabb(camera.minimum, camera.maximum)
                .or_else(|| PortableRayInterval::new(origin, direction_xyz, 0.0, 0.0))
                .ok_or("plan generated an invalid transparent ray")?;
            words.extend_from_slice(&ray.words());
        }
    }
    Ok(words)
}

/// Every layer one level coarser where it can be; `None` when none can.
pub fn coarser_levels(levels: &[u32], level_counts: &[usize]) -> Option<Vec<u32>> {
    let mut changed = false;
    let coarser = levels
        .iter()
        .zip(level_counts)
        .map(|(&level, &count)| {
            if (level as usize + 1) < count {
                changed = true;
                level + 1
            } else {
                level
            }
        })
        .collect();
    changed.then_some(coarser)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(width: u32, height: u32) -> ScenePlan {
        ScenePlan {
            width,
            height,
            camera: RayCamera {
                origin_zyx: [3.0, 2.0, -1.0],
                forward_zyx: [0.0, 0.0, 1.0],
                right_zyx: [1.0, 0.0, 0.0],
                up_zyx: [0.0, 1.0, 0.0],
                focal_scale: 0.2679492,
                translation_xyz: [0.0; 3],
                minimum: [0.0; 3],
                maximum: [6.0, 4.0, 2.0],
            },
            levels: vec![0],
            level_counts: vec![2],
            step: 0.5,
            opacity_reference: 1.0,
            layers: vec![PlanLayer {
                layer_id: 7,
                level: 0,
                dimensions_xyz: [6, 4, 2],
                chunk_shape_xyz: [4, 4, 1],
                minimum: [0.0; 3],
                maximum: [6.0, 4.0, 2.0],
                channels: vec![PlanChannel {
                    source_index: 1,
                    ordinal: 0,
                    tag: PortableResidencyTag::compose(0, 0).unwrap(),
                    owner_base: 1,
                    transfer_min: 0.0,
                    transfer_max: 100.0,
                    transfer_entries: vec![[0, 0, 0, 0], [255, 255, 255, 255]],
                }],
            }],
        }
    }

    fn rays(count: usize) -> Vec<u32> {
        (0..count)
            .flat_map(|_| {
                PortableRayInterval::new([3.0, 2.0, -1.0], [0.0, 0.0, 1.0], 0.0, 4.0)
                    .unwrap()
                    .words()
            })
            .collect()
    }

    #[test]
    fn a_frame_starts_empty_plans_more_on_misses_and_completes_when_nothing_is_missed() {
        let mut client =
            ClientResidency::new(plan(2, 1), &rays(2), ChunkCache::new(1 << 20)).unwrap();
        assert!(
            client.missing_chunks().is_empty(),
            "nothing is planned before the first dispatch"
        );
        // The bootstrap dispatch carries a placeholder page and an empty residency map.
        let dispatch = client.dispatch().unwrap();
        assert_eq!(dispatch.pages[0], vec![0]);
        assert_eq!(dispatch.request_capacity, PAGE_TABLE_CAPACITY);
        // The shader missed chunks 0 and 3 (the grid is 2×1×2 chunks).
        let key = |chunk: u32| {
            PortableFeedbackKey::new(chunk, PortableResidencyTag::compose(0, 0).unwrap())
                .unwrap()
                .packed()
        };
        let mut requests = vec![u32::MAX; PAGE_TABLE_CAPACITY];
        requests[5] = key(3);
        requests[9] = key(0);
        requests[11] = key(3);
        assert_eq!(
            client.absorb_requests(&requests).unwrap(),
            StepOutcome::Planned
        );
        let missing = client.missing_chunks();
        assert_eq!(
            missing
                .iter()
                .map(|m| (
                    m.layer_id,
                    m.level,
                    m.source_index,
                    m.ordinal,
                    m.chunk_index,
                    m.chunk_xyz
                ))
                .collect::<Vec<_>>(),
            vec![(7, 0, 1, 0, 0, [0, 0, 0]), (7, 0, 1, 0, 3, [1, 0, 1])],
            "planned in ascending chunk order, once each"
        );
        assert!(
            client.dispatch().is_err(),
            "planned chunks must be fetched before dispatching"
        );
        // Chunk 0 is a full 4×4×1 chunk; chunk 3 is the x-edge chunk, 2×4×1.
        client.insert_chunk(&missing[0], (0..16).collect());
        client.insert_chunk(&missing[1], (100..108).collect());
        let dispatch = client.dispatch().unwrap();
        assert_eq!(
            dispatch.pages[0].len(),
            24,
            "both chunks in page 0, back to back"
        );
        assert_eq!(&dispatch.pages[0][..16], &(0..16).collect::<Vec<u32>>()[..]);
        assert_eq!(
            &dispatch.pages[0][16..],
            &(100..108).collect::<Vec<u32>>()[..]
        );
        assert!(dispatch.pages[1].is_empty());
        // Nothing missed: complete. Absorbing the same keys again would be a desynchronization.
        assert_eq!(
            client
                .absorb_requests(&vec![u32::MAX; PAGE_TABLE_CAPACITY])
                .unwrap(),
            StepOutcome::Complete
        );
        let cache = client.into_cache();
        assert_eq!((cache.len(), cache.total_words()), (2, 24));
    }

    #[test]
    fn a_request_for_a_channel_the_frame_lacks_is_an_error_and_a_bound_is_reported() {
        let mut client = ClientResidency::new(plan(1, 1), &rays(1), ChunkCache::default()).unwrap();
        let foreign = PortableFeedbackKey::new(0, PortableResidencyTag::compose(3, 0).unwrap())
            .unwrap()
            .packed();
        assert!(client.absorb_requests(&[foreign]).is_err());
        // A chunk larger than a page cannot be planned: the loop reports the bound.
        let mut huge = plan(1, 1);
        huge.layers[0].dimensions_xyz = [2048, 2048, 1];
        huge.layers[0].chunk_shape_xyz = [2048, 2048, 1];
        let mut client = ClientResidency::new(huge, &rays(1), ChunkCache::default()).unwrap();
        let key = PortableFeedbackKey::new(0, PortableResidencyTag::compose(0, 0).unwrap())
            .unwrap()
            .packed();
        assert!(matches!(
            client.absorb_requests(&[key]).unwrap(),
            StepOutcome::ExceedsPortableBound { required_pages: 4 }
        ));
    }

    #[test]
    fn the_cache_evicts_oldest_first_within_its_budget_and_never_starves_the_newest() {
        let mut cache = ChunkCache::new(10);
        let request = |chunk_index: u32| ChunkRequest {
            layer_id: 1,
            level: 0,
            source_index: 0,
            ordinal: 0,
            chunk_index,
            chunk_xyz: [0; 3],
        };
        cache.insert(&request(1), vec![0; 4]);
        cache.insert(&request(2), vec![0; 4]);
        cache.insert(&request(3), vec![0; 4]);
        assert!(
            !cache.contains(&request(1))
                && cache.contains(&request(2))
                && cache.contains(&request(3))
        );
        assert_eq!(cache.total_words(), 8);
        cache.insert(&request(4), vec![0; 40]);
        assert!(
            cache.contains(&request(4)) && cache.len() == 1,
            "an over-budget chunk still lands, alone"
        );
        // Re-inserting refreshes, not duplicates.
        let mut cache = ChunkCache::new(100);
        cache.insert(&request(1), vec![0; 4]);
        cache.insert(&request(1), vec![0; 6]);
        assert_eq!((cache.len(), cache.total_words()), (1, 6));
    }

    #[test]
    fn rays_round_trip_through_their_words_and_the_plan_is_camel_case_json() {
        let ray = PortableRayInterval::new([1.0, 2.0, 3.0], [0.0, 1.0, 0.0], 0.5, 9.0).unwrap();
        let words = ray_words(&[ray]);
        assert_eq!(words.len(), RAY_WORDS);
        assert_eq!(rays_from_words(&words).unwrap(), vec![ray]);
        assert!(rays_from_words(&words[..7]).is_err());
        let json = serde_json::to_value(plan(2, 1)).unwrap();
        assert!(json["layers"][0]["channels"][0].get("ownerBase").is_some());
        assert!(json["levelCounts"].is_array());
        assert_eq!(coarser_levels(&[0, 1], &[2, 2]), Some(vec![1, 1]));
        assert_eq!(coarser_levels(&[1], &[2]), None);
    }
}
