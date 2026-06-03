// marius-render — Shell : I/O, rendu Maud, Dispatcher.

pub mod dispatcher;
pub use dispatcher::{Dispatcher, DispatcherConfig, render_batch, render_batch_pure};
