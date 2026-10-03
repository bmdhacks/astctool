// astctool: turn a folder of textures into an ARMSX2 ASTC tar.zst pack.

pub mod cli;
pub mod contract;
pub mod input;
pub mod kram;
pub mod ktx;
pub mod pipeline;
pub mod report;
pub mod scan;
pub mod tarzstd;

pub use pipeline::{run, PipelineOptions, Progress, Report, Failure};

/// Formats offered in the UI / CLI. All map to kram `-f`.
pub const FORMATS: [&str; 4] = ["astc4x4", "astc5x5", "astc6x6", "astc8x8"];

pub const DEFAULT_FORMAT: &str = "astc6x6";
pub const DEFAULT_QUALITY: i32 = 98;
pub const MAX_MIP_LEVELS: u32 = 7;
