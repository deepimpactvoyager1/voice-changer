//! The real-time effect chain. Pure DSP, no audio hardware or UI.

mod filters;
mod noise;
mod processor;
#[cfg(test)]
mod tests;

pub(crate) use processor::Processor;
