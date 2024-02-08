/// Language specific bindings and loaders are defined here
pub(crate) mod native;

#[cfg(feature = "go")]
pub(crate) mod go;
