use super::*;
use crate::syscalls::*;

/// Wake up all threads that are waiting on futex_wait on this futex.
///
/// ## Parameters
///
/// * `futex` - Memory location that holds a futex that others may be waiting on
pub fn futex_wake_all<M: MemorySize>(
    ctx: FunctionEnvMut<'_, WasiEnv>,
    futex_ptr: WasmPtr<u32, M>,
    ret_woken: WasmPtr<Bool, M>,
) -> Errno {
    let env = ctx.data();
    let memory = env.memory_view(&ctx);
    let state = env.state.deref();

    let pointer: u64 = wasi_try!(futex_ptr.offset().try_into().map_err(|_| Errno::Overflow));
    let mut woken = false;

    let mut guard = state.futexs.read().unwrap();
    if let Some(futex) = guard.get(&pointer) {
        woken = futex.waker.receiver_count() > 0;
        let _ = futex.waker.send(());
        trace!(
            %woken,
            "wasi[{}:{}]::futex_wake_all(offset={})",
            ctx.data().pid(),
            ctx.data().tid(),
            futex_ptr.offset()
        );
    } else {
        trace!(
            "wasi[{}:{}]::futex_wake_all(offset={}) - nothing waiting",
            ctx.data().pid(),
            ctx.data().tid(),
            futex_ptr.offset()
        );
    }

    let woken = match woken {
        false => Bool::False,
        true => Bool::True,
    };
    wasi_try_mem!(ret_woken.write(&memory, woken));

    Errno::Success
}