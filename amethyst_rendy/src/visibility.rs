//! Transparency, visibility sorting and camera centroid culling for 3D Meshes.
use crate::{
    camera::{ActiveCamera, Camera},
    transparent::Transparent,
};
use amethyst_core::{
    ecs::*,
    math::{convert, distance_squared, Matrix4, Point3, Vector4},
    transform::LocalToWorld,
    Hidden, HiddenPropagate,
};

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[cfg(feature = "profiler")]
use thread_profiler::profile_scope;

/// Resource for controlling what entities should be rendered, and whether to draw them ordered or
/// not, which is useful for transparent surfaces.
#[derive(Default, Debug)]
pub struct Visibility {
    /// Visible entities that can be drawn in any order
    pub visible_unordered: IndexSet<Entity>,
    /// Visible entities that need to be drawn in the given order
    pub visible_ordered: Vec<Entity>,
}

/// Holds internal state of the visibility sorting system
#[derive(Default, Debug)]
struct VisibilitySortingSystemState {
    centroids: Vec<Internals>,
    transparent: Vec<Internals>,
}

/// Defines a object's bounding sphere used by frustum culling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundingSphere {
    /// Center of the bounding sphere
    pub center: Point3<f32>,
    /// Radius of the bounding sphere.
    pub radius: f32,
}

impl Default for BoundingSphere {
    fn default() -> Self {
        Self {
            center: Point3::origin(),
            radius: 1.0,
        }
    }
}

impl BoundingSphere {
    /// Create a new `BoundingSphere` with the supplied radius and center.
    pub fn new(center: Point3<f32>, radius: f32) -> Self {
        Self { center, radius }
    }

    /// Returns the center of the sphere.
    pub fn origin(radius: f32) -> Self {
        Self {
            center: Point3::origin(),
            radius,
        }
    }
}

#[derive(Debug, Clone)]
struct Internals {
    entity: Entity,
    transparent: bool,
    centroid: Point3<f32>,
    camera_distance: f32,
}

/// Determine what entities are visible to the camera, and which are not. Will also sort transparent
/// entities back to front based on distance from camera.
///
/// Note that this should run after `Transform` has been updated for the current frame, and
/// before rendering occurs.
pub fn build_visibility_sorting_system() -> impl Runnable {
    let mut state = VisibilitySortingSystemState::default();

    SystemBuilder::new("VisibilitySortingSystem")
        .read_resource::<ActiveCamera>()
        .write_resource::<Visibility>()
        .with_query(<(&Camera, &LocalToWorld)>::query())
        .with_query(<(Entity, &Camera, &LocalToWorld)>::query())
        .with_query(
            <(
                Entity,
                &LocalToWorld,
                Option<&Transparent>,
                Option<&BoundingSphere>,
            )>::query()
            .filter(!component::<Hidden>() & !component::<HiddenPropagate>()),
        )
        .build(
            move |commands,
                  world,
                  (active_camera, visibility),
                  (camera_query1, camera_query2, entity_query)| {
                #[cfg(feature = "profiler")]
                profile_scope!("visibility_sorting_system");

                visibility.visible_unordered.clear();
                visibility.visible_ordered.clear();
                state.transparent.clear();
                state.centroids.clear();

                let origin = Point3::origin();

                let (camera, camera_transform) = match active_camera.entity.map_or_else(
                    || camera_query1.iter(world).nth(0),
                    |e| {
                        camera_query2
                            .iter(world)
                            .find(|(camera_entity, _, _)| **camera_entity == e)
                            .map(|(_entity, camera, camera_transform)| (camera, camera_transform))
                    },
                ) {
                    Some(r) => r,
                    None => return,
                };

                let camera_centroid = camera_transform.transform_point(&origin);
                let frustum = Frustum::new(
                    convert::<_, Matrix4<f32>>(camera.matrix)
                        * camera_transform.try_inverse().unwrap(),
                );

                state.centroids.extend(
                    entity_query
                        .iter(world)
                        .map(|(entity, transform, transparent, sphere)| {
                            let pos = sphere.clone().map_or(origin, |s| s.center);
                            (
                                *entity,
                                transparent.is_some(),
                                transform.transform_point(&pos),
                                sphere.map_or(1.0, |s| s.radius)
                                    * transform[(0, 0)]
                                        .max(transform[(1, 1)])
                                        .max(transform[(2, 2)]),
                            )
                        })
                        .filter(|(_, _, centroid, radius)| frustum.check_sphere(centroid, *radius))
                        .map(|(entity, transparent, centroid, _)| Internals {
                            entity,
                            transparent,
                            centroid,
                            camera_distance: distance_squared(&centroid, &camera_centroid),
                        }),
                );

                state
                    .transparent
                    .extend(state.centroids.iter().filter(|c| c.transparent).cloned());

                state.transparent.sort_by(|a, b| {
                    b.camera_distance
                        .partial_cmp(&a.camera_distance)
                        .unwrap_or(Ordering::Equal)
                });

                visibility.visible_unordered.extend(
                    state
                        .centroids
                        .iter()
                        .filter(|c| !c.transparent)
                        .map(|c| c.entity),
                );

                visibility
                    .visible_ordered
                    .extend(state.transparent.iter().map(|c| c.entity));
            },
        )
}

/// Simple view Frustum implementation
#[derive(Debug)]
pub struct Frustum {
    /// The planes of the frustum
    pub planes: [Vector4<f32>; 6],
}

impl Frustum {
    /// Create a new simple frustum from the provided matrix.
    pub fn new(matrix: Matrix4<f32>) -> Self {
        let planes = [
            (matrix.row(3) + matrix.row(0)).transpose(),
            (matrix.row(3) - matrix.row(0)).transpose(),
            (matrix.row(3) - matrix.row(1)).transpose(),
            (matrix.row(3) + matrix.row(1)).transpose(),
            (matrix.row(3) + matrix.row(2)).transpose(),
            (matrix.row(3) - matrix.row(2)).transpose(),
        ];
        Self {
            planes: [
                planes[0] * (1.0 / planes[0].xyz().magnitude()),
                planes[1] * (1.0 / planes[1].xyz().magnitude()),
                planes[2] * (1.0 / planes[2].xyz().magnitude()),
                planes[3] * (1.0 / planes[3].xyz().magnitude()),
                planes[4] * (1.0 / planes[4].xyz().magnitude()),
                planes[5] * (1.0 / planes[5].xyz().magnitude()),
            ],
        }
    }

    /// Check if the given sphere is within the Frustum
    pub fn check_sphere(&self, center: &Point3<f32>, radius: f32) -> bool {
        for plane in &self.planes {
            if plane.xyz().dot(&center.coords) + plane.w <= -radius {
                return false;
            }
        }
        true
    }
}
