use std::sync::atomic::AtomicBool;

use common::types::PointOffsetType;

use crate::common::check_stopped;
use crate::common::operation_error::OperationResult;
use crate::index::hnsw_index::gpu::batched_points::BatchedPoints;
use crate::index::hnsw_index::gpu::create_graph_layers_builder;
use crate::index::hnsw_index::gpu::gpu_insert_context::GpuInsertContext;
use crate::index::hnsw_index::gpu::gpu_level_builder::build_level_on_gpu;
use crate::index::hnsw_index::graph_layers_builder::GraphLayersBuilder;
use crate::index::hnsw_index::point_scorer::FilteredScorer;

/// Maximum count of point IDs per visited flag.
pub static GPU_MAX_VISITED_FLAGS_FACTOR: usize = 32;

/// Build HNSW graph on GPU.
#[allow(clippy::too_many_arguments)]
pub fn build_hnsw_on_gpu<'a, 'b>(
    gpu_insert_context: &mut GpuInsertContext<'b>,
    // Graph with all settings like m, ef, levels, etc.
    reference_graph: &GraphLayersBuilder,
    // Parallel inserts count.
    groups_count: usize,
    // Number of entry points of hnsw graph.
    entry_points_num: usize,
    // Amount of first points to link on CPU.
    cpu_linked_points: usize,
    // Point IDs to insert.
    // In payload blocks we need to use subset of all points.
    ids: Vec<PointOffsetType>,
    // Scorer builder for CPU build.
    points_scorer_builder: impl Fn(PointOffsetType) -> OperationResult<FilteredScorer<'a>> + Send + Sync,
    stopped: &AtomicBool,
) -> OperationResult<GraphLayersBuilder> {
    let num_vectors = reference_graph.links_layers().len();
    let hnsw_m = reference_graph.hnsw_m();
    let ef = std::cmp::max(reference_graph.ef_construct(), hnsw_m.m0);

    // Divide points into batches.
    // One batch is one shader invocation.
    let batched_points = BatchedPoints::new(
        |point_id| reference_graph.get_point_level(point_id),
        ids,
        groups_count,
    )?;

    let mut graph_layers_builder =
        create_graph_layers_builder(&batched_points, num_vectors, hnsw_m, ef, entry_points_num);

    // Link first points on CPU.
    let mut cpu_linked_points_count = 0;
    for batch in batched_points.iter_batches(0) {
        for point in batch.points {
            check_stopped(stopped)?;
            let points_scorer = points_scorer_builder(point.point_id)?;
            graph_layers_builder.link_new_point(point.point_id, points_scorer);
            cpu_linked_points_count += 1;
            if cpu_linked_points_count >= cpu_linked_points {
                break;
            }
        }
        if cpu_linked_points_count >= cpu_linked_points {
            break;
        }
    }

    // Mark all points as ready, as GPU will fill layer by layer.
    graph_layers_builder.fill_ready_list();

    // Check if all points are linked on CPU.
    // If there are no batches left, we can return result before gpu resources creation.
    if batched_points
        .iter_batches(cpu_linked_points_count)
        .next()
        .is_none()
    {
        return Ok(graph_layers_builder);
    }

    gpu_insert_context.init(batched_points.remap())?;

    // Build all levels on GPU level by level.
    for level in (0..batched_points.levels_count()).rev() {
        log::trace!("Starting GPU level {level}");

        gpu_insert_context.upload_links(level, &graph_layers_builder, stopped)?;
        build_level_on_gpu(
            gpu_insert_context,
            &batched_points,
            cpu_linked_points,
            level,
            stopped,
        )?;
        gpu_insert_context.download_links(level, &graph_layers_builder, stopped)?;
    }

    gpu_insert_context.log_measurements();

    Ok(graph_layers_builder)
}

#[cfg(test)]
mod tests {
    use std::borrow::Borrow;

    use super::*;
    use crate::index::hnsw_index::HnswM;
    use crate::index::hnsw_index::gpu::gpu_vector_storage::GpuVectorStorage;
    use crate::index::hnsw_index::gpu::tests::{
        GpuGraphTestData, check_graph_layers_builders_quality, compare_graph_layers_builders,
        create_gpu_graph_test_data,
    };
    use crate::vector_storage::chunked_vector_storage::VectorOffsetType;

    fn build_gpu_graph(
        test: &GpuGraphTestData,
        groups_count: usize,
        cpu_linked_points_count: usize,
        exact: bool,
        repeats: usize,
    ) -> Vec<GraphLayersBuilder> {
        let num_vectors = test.graph_layers_builder.links_layers().len();
        let instance = gpu::GPU_TEST_INSTANCE.clone();
        let device = gpu::Device::new(instance.clone(), &instance.physical_devices()[0]).unwrap();

        let gpu_vector_storage = GpuVectorStorage::new(
            device.clone(),
            test.vector_storage.borrow(),
            None,
            false,
            &false.into(),
        )
        .unwrap();

        let mut gpu_search_context = GpuInsertContext::new(
            &gpu_vector_storage,
            groups_count,
            test.graph_layers_builder.hnsw_m(),
            test.graph_layers_builder.ef_construct(),
            exact,
            1..=GPU_MAX_VISITED_FLAGS_FACTOR,
        )
        .unwrap();

        let ids: Vec<_> = (0..num_vectors as PointOffsetType).collect();

        (0..repeats)
            .map(|_| {
                build_hnsw_on_gpu(
                    &mut gpu_search_context,
                    &test.graph_layers_builder,
                    groups_count,
                    1,
                    cpu_linked_points_count,
                    ids.clone(),
                    |point_id| {
                        let added_vector = test
                            .vector_holder
                            .vectors
                            .get(point_id as VectorOffsetType)
                            .to_vec();
                        Ok(test.vector_holder.get_scorer(added_vector.clone()))
                    },
                    &false.into(),
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn test_gpu_hnsw_equivalency() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let num_vectors = 1024;
        let dim = 64;
        let hnsw_m = HnswM::new2(8);
        let ef = 32;
        let min_cpu_linked_points_count = 64;

        let test = create_gpu_graph_test_data(num_vectors, dim, hnsw_m, ef, 0);
        let graph_layers_builders = build_gpu_graph(&test, 1, min_cpu_linked_points_count, true, 2);

        for graph_layers_builder in graph_layers_builders.iter() {
            compare_graph_layers_builders(&test.graph_layers_builder, graph_layers_builder);
        }
    }

    #[test]
    fn test_gpu_hnsw_quality_exact() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let num_vectors = 1024;
        let dim = 64;
        let hnsw_m = HnswM::new2(8);
        let ef = 32;
        let groups_count = 4;
        let searches_count = 20;
        let top = 10;
        let min_cpu_linked_points_count = 64;

        let test = create_gpu_graph_test_data(num_vectors, dim, hnsw_m, ef, searches_count);
        let graph_layers_builders =
            build_gpu_graph(&test, groups_count, min_cpu_linked_points_count, true, 1);

        let graph_layers_builder = graph_layers_builders.into_iter().next().unwrap();
        check_graph_layers_builders_quality(graph_layers_builder, test, top, ef, 0.8)
    }

    #[test]
    fn test_gpu_hnsw_quality() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let num_vectors = 1024;
        let dim = 64;
        let hnsw_m = HnswM::new2(8);
        let ef = 32;
        let groups_count = 4;
        let searches_count = 20;
        let top = 10;
        let min_cpu_linked_points_count = 64;

        let test = create_gpu_graph_test_data(num_vectors, dim, hnsw_m, ef, searches_count);
        let graph_layers_builders =
            build_gpu_graph(&test, groups_count, min_cpu_linked_points_count, false, 1);

        let graph_layers_builder = graph_layers_builders.into_iter().next().unwrap();
        check_graph_layers_builders_quality(graph_layers_builder, test, top, ef, 0.8)
    }
}
