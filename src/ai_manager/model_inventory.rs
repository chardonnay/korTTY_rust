//! Cancellable background model inventory loads for the AI Manager.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::Task;

pub(super) struct ModelInventory(Arc<AtomicBool>);

impl ModelInventory {
    pub(super) fn start<T: Send + 'static, M: Send + 'static>(
        load: impl FnOnce(&AtomicBool) -> T + Send + 'static,
        unavailable: impl FnOnce() -> T + Send + 'static,
        complete: impl FnOnce(T) -> M + Send + 'static,
    ) -> (Self, Task<M>) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancelled);
        let task = Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                if !token.load(Ordering::Acquire)
                    && std::thread::Builder::new()
                        .name("kortty-model-inventory".into())
                        .spawn(move || {
                            let _ = send.try_send(load(&token));
                        })
                        .is_ok()
                    && let Ok(inventory) = receive.recv().await
                {
                    return inventory;
                }
                unavailable()
            },
            complete,
        );
        (Self(cancelled), task)
    }
}

impl Drop for ModelInventory {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{StreamExt, executor::block_on};

    #[test]
    fn policy_model_inventory_does_not_start_after_the_dialog_closes() {
        let (inventory, task) = ModelInventory::start(
            |_| panic!("the closed dialog should cancel inventory acquisition"),
            || "cancelled",
            |result| result,
        );
        drop(inventory);
        let mut output = iced_runtime::task::into_stream(task).unwrap();
        assert!(matches!(
            block_on(output.next()),
            Some(iced_runtime::Action::Output("cancelled"))
        ));
    }
}
