//! Where a client's project reads come from.
//!
//! A window over a local project calls the readers directly. A window over a
//! project on another machine (the remote window) runs the same readers on
//! the far machine, inside its `shelbi relay`, and gets their answers back
//! over the relay's `query` service. [`wire`] is that service's message shape.
//! The read seam itself (`ProjectSource`) builds on it.

pub mod wire;
