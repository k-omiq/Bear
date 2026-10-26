//! The engine ladder.
//!
//! Rungs 0 and 2 are here; the rungs above them arrive with the phases that
//! build them. What they share is this shape - given a page and a
//! [`crate::fit::Fitted`], produce the pixels for one patch covering the mask's
//! bounds, in the page's own mode and depth, with everything outside the mask
//! copied through.
//!
//! The model rungs differ from the arithmetic ones in two ways the shape does
//! not show. They own a **held session** rather than being a free function, so
//! [`lama::Inpainter`] is a value the job keeps and the region borrows; and
//! they can **decline** - refuse a region and leave it as it was - which the
//! arithmetic rungs never do.
//!
//! ## One model rung, and [`model`] beside it
//!
//! [`lama`] is the inpainter, and it is the only in-process model rung. There
//! used to be a second - MI-GAN, an optional fast preview tier - and it was
//! removed outright: it was a fourth rung nothing ever *started* on, reachable
//! only by escalation from rung 2, and every region that got there had already
//! been declined by a stronger model. A rung whose only job is to be tried
//! after the better engine gave up is a 28 MB download and a match arm buying
//! nothing.
//!
//! [`model`] stays. It holds the geometry both model rungs shared - the
//! tiling, the applied mask, the write bound, the decline vocabulary and the
//! isolation ramp - and that geometry is the window rule's, which sizes the
//! decode window against a 512² `engine_context` and
//! tiles anything larger with 128 px of overlap. It is a rule about the window
//! rather than about how many engines look through it, and rung 3a
//! ([`flux`]) reads the same module.

/// The Gradient tool's Auto type: the surface all round a selection, blended in.
pub mod autogradient;
pub mod fill;
/// Rung 3a, the out-of-process one. It is here because it is a rung and it
/// keeps the rungs' shape - [`flux::applies`], [`flux::declines`],
/// `Inpainter::render`, a [`model::Rendered`] - but almost none of it is in
/// this file's neighbourhood: the transport, the lifecycle and the hardware
/// gate are [`crate::sidecar`], and this is the part that turns a region into a
/// crop and a reply into a patch.
pub mod flux;
/// The Gradient tool: evaluate, fit to the surroundings, dither.
pub mod gradient;
/// Grids and image operations for the arithmetic fills below.
pub mod grid;
pub mod lama;
pub mod model;
/// Multiscale PatchMatch completion: the Patch tool's Content-Aware mode.
pub mod patchmatch;
/// The Patch tool's proposed source: the offset whose surroundings match.
pub mod patchsource;
/// Membrane and Poisson solves: the Patch tool's Normal mode and Smooth.
pub mod poisson;
pub mod render;
/// The screentone fill: lettering on a halftone screen, dots kept in phase.
pub mod screentone;

pub use render::{CloudProvider, ExecutionTarget, GeneratedCrop, PreparedRender, RenderRecipe};
