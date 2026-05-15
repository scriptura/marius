// marius-collector
// Crate Core — attitude no_std.
// Contient : Collector<MAX>, trait Projection, Dispatcher.

pub mod collector;
pub mod projection;
pub mod dispatcher;

pub use collector::Collector;
pub use projection::Projection;
pub use dispatcher::Dispatcher;
