// Copyright 2012 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[doc(hidden)];

use cast;
use iter;
use libc;
use option;
use comm::{GenericChan, GenericPort};
use prelude::*;
use ptr;
use result;
use task;
use task::{TaskBuilder, atomically};
use uint;

#[path = "private/at_exit.rs"]
pub mod at_exit;
#[path = "private/global.rs"]
pub mod global;
#[path = "private/finally.rs"]
pub mod finally;
#[path = "private/weak_task.rs"]
pub mod weak_task;
#[path = "private/exchange_alloc.rs"]
pub mod exchange_alloc;
#[path = "private/intrinsics.rs"]
pub mod intrinsics;
#[path = "private/extfmt.rs"]
pub mod extfmt;

extern mod rustrt {
    pub unsafe fn rust_create_little_lock() -> rust_little_lock;
    pub unsafe fn rust_destroy_little_lock(lock: rust_little_lock);
    pub unsafe fn rust_lock_little_lock(lock: rust_little_lock);
    pub unsafe fn rust_unlock_little_lock(lock: rust_little_lock);

    pub unsafe fn rust_raw_thread_start(f: &fn()) -> *raw_thread;
    pub unsafe fn rust_raw_thread_join_delete(thread: *raw_thread);
}

#[allow(non_camel_case_types)] // runtime type
type raw_thread = libc::c_void;

/**

Start a new thread outside of the current runtime context and wait
for it to terminate.

The executing thread has no access to a task pointer and will be using
a normal large stack.
*/
pub unsafe fn run_in_bare_thread(f: ~fn()) {
    let (port, chan) = comm::stream();
    // FIXME #4525: Unfortunate that this creates an extra scheduler but it's
    // necessary since rust_raw_thread_join_delete is blocking
    do task::spawn_sched(task::SingleThreaded) {
        unsafe {
            let closure: &fn() = || {
                f()
            };
            let thread = rustrt::rust_raw_thread_start(closure);
            rustrt::rust_raw_thread_join_delete(thread);
            chan.send(());
        }
    }
    port.recv();
}

#[test]
fn test_run_in_bare_thread() {
    unsafe {
        let i = 100;
        do run_in_bare_thread {
            assert i == 100;
        }
    }
}

#[test]
fn test_run_in_bare_thread_exchange() {
    unsafe {
        // Does the exchange heap work without the runtime?
        let i = ~100;
        do run_in_bare_thread {
            assert i == ~100;
        }
    }
}

fn compare_and_swap(address: &mut int, oldval: int, newval: int) -> bool {
    unsafe {
        let old = intrinsics::atomic_cxchg(address, oldval, newval);
        old == oldval
    }
}

/****************************************************************************
 * Shared state & exclusive ARC
 ****************************************************************************/

// An unwrapper uses this protocol to communicate with the "other" task that
// drops the last refcount on an arc. Unfortunately this can't be a proper
// pipe protocol because the unwrapper has to access both stages at once.
type UnwrapProto = ~mut Option<(comm::ChanOne<()>,  comm::PortOne<bool>)>;

struct ArcData<T> {
    mut count:     libc::intptr_t,
    mut unwrapper: int, // either a UnwrapProto or 0
    // FIXME(#3224) should be able to make this non-option to save memory, and
    // in unwrap() use "let ~ArcData { data: result, _ } = thing" to unwrap it
    mut data:      Option<T>,
}

struct ArcDestruct<T> {
    mut data: *libc::c_void,
    drop {
        unsafe {
            if self.data.is_null() {
                return; // Happens when destructing an unwrapper's handle.
            }
            do task::unkillable {
                let data: ~ArcData<T> = cast::reinterpret_cast(&self.data);
                let new_count =
                    intrinsics::atomic_xsub(&mut data.count, 1) - 1;
                assert new_count >= 0;
                if new_count == 0 {
                    // Were we really last, or should we hand off to an
                    // unwrapper? It's safe to not xchg because the unwrapper
                    // will set the unwrap lock *before* dropping his/her
                    // reference. In effect, being here means we're the only
                    // *awake* task with the data.
                    if data.unwrapper != 0 {
                        let p: UnwrapProto =
                            cast::reinterpret_cast(&data.unwrapper);
                        let (message, response) = option::swap_unwrap(p);
                        // Send 'ready' and wait for a response.
                        comm::send_one(message, ());
                        // Unkillable wait. Message guaranteed to come.
                        if comm::recv_one(response) {
                            // Other task got the data.
                            cast::forget(data);
                        } else {
                            // Other task was killed. drop glue takes over.
                        }
                    } else {
                        // drop glue takes over.
                    }
                } else {
                    cast::forget(data);
                }
            }
        }
    }
}

fn ArcDestruct<T>(data: *libc::c_void) -> ArcDestruct<T> {
    ArcDestruct {
        data: data
    }
}

pub unsafe fn unwrap_shared_mutable_state<T:Owned>(rc: SharedMutableState<T>)
        -> T {
    struct DeathThroes<T> {
        mut ptr:      Option<~ArcData<T>>,
        mut response: Option<comm::ChanOne<bool>>,
        drop {
            unsafe {
                let response = option::swap_unwrap(&mut self.response);
                // In case we get killed early, we need to tell the person who
                // tried to wake us whether they should hand-off the data to
                // us.
                if task::failing() {
                    comm::send_one(response, false);
                    // Either this swap_unwrap or the one below (at "Got
                    // here") ought to run.
                    cast::forget(option::swap_unwrap(&mut self.ptr));
                } else {
                    assert self.ptr.is_none();
                    comm::send_one(response, true);
                }
            }
        }
    }

    do task::unkillable {
        let ptr: ~ArcData<T> = cast::reinterpret_cast(&rc.data);
        let (p1,c1) = comm::oneshot(); // ()
        let (p2,c2) = comm::oneshot(); // bool
        let server: UnwrapProto = ~mut Some((c1,p2));
        let serverp: int = cast::transmute(server);
        // Try to put our server end in the unwrapper slot.
        if compare_and_swap(&mut ptr.unwrapper, 0, serverp) {
            // Got in. Step 0: Tell destructor not to run. We are now it.
            rc.data = ptr::null();
            // Step 1 - drop our own reference.
            let new_count = intrinsics::atomic_xsub(&mut ptr.count, 1) - 1;
            //assert new_count >= 0;
            if new_count == 0 {
                // We were the last owner. Can unwrap immediately.
                // Also we have to free the server endpoints.
                let _server: UnwrapProto = cast::transmute(serverp);
                option::swap_unwrap(&mut ptr.data)
                // drop glue takes over.
            } else {
                // The *next* person who sees the refcount hit 0 will wake us.
                let end_result =
                    DeathThroes { ptr: Some(ptr),
                                  response: Some(c2) };
                let mut p1 = Some(p1); // argh
                do task::rekillable {
                    comm::recv_one(option::swap_unwrap(&mut p1));
                }
                // Got here. Back in the 'unkillable' without getting killed.
                // Recover ownership of ptr, then take the data out.
                let ptr = option::swap_unwrap(&mut end_result.ptr);
                option::swap_unwrap(&mut ptr.data)
                // drop glue takes over.
            }
        } else {
            // Somebody else was trying to unwrap. Avoid guaranteed deadlock.
            cast::forget(ptr);
            // Also we have to free the (rejected) server endpoints.
            let _server: UnwrapProto = cast::transmute(serverp);
            fail!(~"Another task is already unwrapping this ARC!");
        }
    }
}

/**
 * COMPLETELY UNSAFE. Used as a primitive for the safe versions in std::arc.
 *
 * Data races between tasks can result in crashes and, with sufficient
 * cleverness, arbitrary type coercion.
 */
pub type SharedMutableState<T> = ArcDestruct<T>;

pub unsafe fn shared_mutable_state<T:Owned>(data: T) ->
        SharedMutableState<T> {
    let data = ~ArcData { count: 1, unwrapper: 0, data: Some(data) };
    unsafe {
        let ptr = cast::transmute(data);
        ArcDestruct(ptr)
    }
}

#[inline(always)]
pub unsafe fn get_shared_mutable_state<T:Owned>(
    rc: *SharedMutableState<T>) -> *mut T
{
    unsafe {
        let ptr: ~ArcData<T> = cast::reinterpret_cast(&(*rc).data);
        assert ptr.count > 0;
        let r = cast::transmute(option::get_ref(&ptr.data));
        cast::forget(ptr);
        return r;
    }
}
#[inline(always)]
pub unsafe fn get_shared_immutable_state<T:Owned>(
        rc: &a/SharedMutableState<T>) -> &a/T {
    unsafe {
        let ptr: ~ArcData<T> = cast::reinterpret_cast(&(*rc).data);
        assert ptr.count > 0;
        // Cast us back into the correct region
        let r = cast::transmute_region(option::get_ref(&ptr.data));
        cast::forget(ptr);
        return r;
    }
}

pub unsafe fn clone_shared_mutable_state<T:Owned>(rc: &SharedMutableState<T>)
        -> SharedMutableState<T> {
    unsafe {
        let ptr: ~ArcData<T> = cast::reinterpret_cast(&(*rc).data);
        let new_count = intrinsics::atomic_xadd(&mut ptr.count, 1) + 1;
        assert new_count >= 2;
        cast::forget(ptr);
    }
    ArcDestruct((*rc).data)
}

impl<T:Owned> Clone for SharedMutableState<T> {
    fn clone(&self) -> SharedMutableState<T> {
        unsafe {
            clone_shared_mutable_state(self)
        }
    }
}

/****************************************************************************/

#[allow(non_camel_case_types)] // runtime type
type rust_little_lock = *libc::c_void;

struct LittleLock {
    l: rust_little_lock,
    drop {
        unsafe {
            rustrt::rust_destroy_little_lock(self.l);
        }
    }
}

fn LittleLock() -> LittleLock {
    unsafe {
        LittleLock {
            l: rustrt::rust_create_little_lock()
        }
    }
}

impl LittleLock {
    #[inline(always)]
    unsafe fn lock<T>(f: fn() -> T) -> T {
        struct Unlock {
            l: rust_little_lock,
            drop {
                unsafe {
                    rustrt::rust_unlock_little_lock(self.l);
                }
            }
        }

        fn Unlock(l: rust_little_lock) -> Unlock {
            Unlock {
                l: l
            }
        }

        do atomically {
            rustrt::rust_lock_little_lock(self.l);
            let _r = Unlock(self.l);
            f()
        }
    }
}

struct ExData<T> { lock: LittleLock, mut failed: bool, mut data: T, }
/**
 * An arc over mutable data that is protected by a lock. For library use only.
 */
pub struct Exclusive<T> { x: SharedMutableState<ExData<T>> }

pub fn exclusive<T:Owned>(user_data: T) -> Exclusive<T> {
    let data = ExData {
        lock: LittleLock(), mut failed: false, mut data: user_data
    };
    Exclusive { x: unsafe { shared_mutable_state(data) } }
}

impl<T:Owned> Clone for Exclusive<T> {
    // Duplicate an exclusive ARC, as std::arc::clone.
    fn clone(&self) -> Exclusive<T> {
        Exclusive { x: unsafe { clone_shared_mutable_state(&self.x) } }
    }
}

impl<T:Owned> Exclusive<T> {
    // Exactly like std::arc::mutex_arc,access(), but with the little_lock
    // instead of a proper mutex. Same reason for being unsafe.
    //
    // Currently, scheduling operations (i.e., yielding, receiving on a pipe,
    // accessing the provided condition variable) are prohibited while inside
    // the exclusive. Supporting that is a work in progress.
    #[inline(always)]
    unsafe fn with<U>(f: fn(x: &mut T) -> U) -> U {
        unsafe {
            let rec = get_shared_mutable_state(&self.x);
            do (*rec).lock.lock {
                if (*rec).failed {
                    fail!(
                        ~"Poisoned exclusive - another task failed inside!");
                }
                (*rec).failed = true;
                let result = f(&mut (*rec).data);
                (*rec).failed = false;
                result
            }
        }
    }

    #[inline(always)]
    unsafe fn with_imm<U>(f: fn(x: &T) -> U) -> U {
        do self.with |x| {
            f(cast::transmute_immut(x))
        }
    }
}

// FIXME(#3724) make this a by-move method on the exclusive
pub fn unwrap_exclusive<T:Owned>(arc: Exclusive<T>) -> T {
    let Exclusive { x: x } = arc;
    let inner = unsafe { unwrap_shared_mutable_state(x) };
    let ExData { data: data, _ } = inner;
    data
}

#[cfg(test)]
pub mod tests {
    use core::option::{None, Some};

    use option;
    use comm;
    use private::{exclusive, unwrap_exclusive};
    use result;
    use task;
    use uint;

    #[test]
    pub fn exclusive_arc() {
        let mut futures = ~[];

        let num_tasks = 10;
        let count = 10;

        let total = exclusive(~mut 0);

        for uint::range(0, num_tasks) |_i| {
            let total = total.clone();
            let (port, chan) = comm::stream();
            futures.push(port);

            do task::spawn || {
                for uint::range(0, count) |_i| {
                    do total.with |count| {
                        **count += 1;
                    }
                }
                chan.send(());
            }
        };

        for futures.each |f| { f.recv() }

        do total.with |total| {
            assert **total == num_tasks * count
        };
    }

    #[test] #[should_fail] #[ignore(cfg(windows))]
    pub fn exclusive_poison() {
        // Tests that if one task fails inside of an exclusive, subsequent
        // accesses will also fail.
        let x = exclusive(1);
        let x2 = x.clone();
        do task::try || {
            do x2.with |one| {
                assert *one == 2;
            }
        };
        do x.with |one| {
            assert *one == 1;
        }
    }

    #[test]
    pub fn exclusive_unwrap_basic() {
        let x = exclusive(~~"hello");
        assert unwrap_exclusive(x) == ~~"hello";
    }

    #[test]
    pub fn exclusive_unwrap_contended() {
        let x = exclusive(~~"hello");
        let x2 = ~mut Some(x.clone());
        do task::spawn || {
            let x2 = option::swap_unwrap(x2);
            do x2.with |_hello| { }
            task::yield();
        }
        assert unwrap_exclusive(x) == ~~"hello";

        // Now try the same thing, but with the child task blocking.
        let x = exclusive(~~"hello");
        let x2 = ~mut Some(x.clone());
        let mut res = None;
        do task::task().future_result(|+r| res = Some(r)).spawn
              || {
            let x2 = option::swap_unwrap(x2);
            assert unwrap_exclusive(x2) == ~~"hello";
        }
        // Have to get rid of our reference before blocking.
        { let _x = x; } // FIXME(#3161) util::ignore doesn't work here
        let res = option::swap_unwrap(&mut res);
        res.recv();
    }

    #[test] #[should_fail] #[ignore(cfg(windows))]
    pub fn exclusive_unwrap_conflict() {
        let x = exclusive(~~"hello");
        let x2 = ~mut Some(x.clone());
        let mut res = None;
        do task::task().future_result(|+r| res = Some(r)).spawn
           || {
            let x2 = option::swap_unwrap(x2);
            assert unwrap_exclusive(x2) == ~~"hello";
        }
        assert unwrap_exclusive(x) == ~~"hello";
        let res = option::swap_unwrap(&mut res);
        // See #4689 for why this can't be just "res.recv()".
        assert res.recv() == task::Success;
    }

    #[test] #[ignore(cfg(windows))]
    pub fn exclusive_unwrap_deadlock() {
        // This is not guaranteed to get to the deadlock before being killed,
        // but it will show up sometimes, and if the deadlock were not there,
        // the test would nondeterministically fail.
        let result = do task::try {
            // a task that has two references to the same exclusive will
            // deadlock when it unwraps. nothing to be done about that.
            let x = exclusive(~~"hello");
            let x2 = x.clone();
            do task::spawn {
                for 10.times { task::yield(); } // try to let the unwrapper go
                fail!(); // punt it awake from its deadlock
            }
            let _z = unwrap_exclusive(x);
            do x2.with |_hello| { }
        };
        assert result.is_err();
    }
}
