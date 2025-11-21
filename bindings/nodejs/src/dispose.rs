use napi::check_status;
use napi::threadsafe_function::ThreadsafeFunctionHandle;

pub trait DisposeThreadsafeHandler {
    fn dispose(&self) -> napi::Result<()>;
}

impl DisposeThreadsafeHandler for ThreadsafeFunctionHandle {
    fn dispose(&self) -> napi::Result<()> {
        self.with_write_aborted(|mut aborted_guard| {
            if !*aborted_guard {
                check_status!(unsafe {
                    napi_sys::napi_release_threadsafe_function(
                        self.get_raw(),
                        napi_sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                })?;
                *aborted_guard = true;
            }

            Ok(())
        })
    }
}
