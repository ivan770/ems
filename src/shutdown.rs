use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::db::HandlerDatabase;

/// Graceful shutdown for EMS
///
/// Relies on registered handlers in [`HandlerDatabase`],
/// so you must ensure, that all handlers are dropped correctly
/// using [`Drop`] or [`remove_handler`].
///
/// [`remove_handler`]: HandlerDatabase::remove_handler
pub struct Shutdown<'d> {
    database: &'d HandlerDatabase,
}

impl<'d> From<&'d HandlerDatabase> for Shutdown<'d> {
    fn from(database: &'d HandlerDatabase) -> Self {
        Shutdown { database }
    }
}

impl<'d> Future for Shutdown<'d> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.get_mut().database.is_empty() {
            Poll::Ready(())
        } else {
            ctx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}
