use std::{error::Error, future::Future};

/// Create a [`Service`] from provided config, possibly failing to do so.
///
/// This is an async version of [`TryFrom`].
///
/// [`Service`]: crate::service::Service
/// [`TryFrom`]: std::convert::TryFrom
pub trait FromConfig<'c>
where
    Self: Sized,
{
    type Config: 'c;

    type Error: Error + Send + Sync + 'static;

    type Fut: Future<Output = Result<Self, Self::Error>>;

    fn from_config(config: Self::Config) -> Self::Fut;
}
