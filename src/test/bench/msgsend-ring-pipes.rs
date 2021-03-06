// Copyright 2012 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// This test creates a bunch of tasks that simultaneously send to each
// other in a ring. The messages should all be basically
// independent. It's designed to hammer the global kernel lock, so
// that things will look really good once we get that lock out of the
// message path.

// This version uses automatically compiled channel contracts.

extern mod std;
use std::time;
use std::future;

use core::pipes::recv;

proto! ring (
    num:send {
        num(uint) -> num
    }
)

macro_rules! move_out (
    ($x:expr) => { unsafe { let y = *ptr::addr_of(&$x); y } }
)

fn thread_ring(i: uint,
               count: uint,
               +num_chan: ring::client::num,
               +num_port: ring::server::num) {
    let mut num_chan = Some(num_chan);
    let mut num_port = Some(num_port);
    // Send/Receive lots of messages.
    for uint::range(0, count) |j| {
        //error!("task %?, iter %?", i, j);
        let mut num_chan2 = None;
        let mut num_port2 = None;
        num_chan2 <-> num_chan;
        num_port2 <-> num_port;
        num_chan = Some(ring::client::num(option::unwrap(num_chan2), i * j));
        let port = option::unwrap(num_port2);
        match recv(port) {
          ring::num(_n, p) => {
            //log(error, _n);
            num_port = Some(move_out!(p));
          }
        }
    };
}

fn main() {
    let args = os::args();
    let args = if os::getenv(~"RUST_BENCH").is_some() {
        ~[~"", ~"100", ~"10000"]
    } else if args.len() <= 1u {
        ~[~"", ~"100", ~"1000"]
    } else {
        copy args
    }; 

    let num_tasks = uint::from_str(args[1]).get();
    let msg_per_task = uint::from_str(args[2]).get();

    let (num_chan, num_port) = ring::init();
    let mut num_chan = Some(num_chan);

    let start = time::precise_time_s();

    // create the ring
    let mut futures = ~[];

    for uint::range(1u, num_tasks) |i| {
        //error!("spawning %?", i);
        let (new_chan, num_port) = ring::init();
        let num_chan2 = ~mut None;
        *num_chan2 <-> num_chan;
        let num_port = ~mut Some(num_port);
        let new_future = do future::spawn || {
            let mut num_chan = None;
            num_chan <-> *num_chan2;
            let mut num_port1 = None;
            num_port1 <-> *num_port;
            thread_ring(i, msg_per_task,
                        option::unwrap(num_chan),
                        option::unwrap(num_port1))
        };
        futures.push(new_future);
        num_chan = Some(new_chan);
    };

    // do our iteration
    thread_ring(0, msg_per_task, option::unwrap(num_chan), num_port);

    // synchronize
    for futures.each |f| { f.get() };

    let stop = time::precise_time_s();

    // all done, report stats.
    let num_msgs = num_tasks * msg_per_task;
    let elapsed = (stop - start);
    let rate = (num_msgs as float) / elapsed;

    io::println(fmt!("Sent %? messages in %? seconds",
                     num_msgs, elapsed));
    io::println(fmt!("  %? messages / second", rate));
    io::println(fmt!("  %? μs / message", 1000000. / rate));
}
