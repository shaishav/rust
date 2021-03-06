// Copyright 2012 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// xfail-fast

fn iter_vec<T>(v: ~[T], f: fn(&T)) { for v.each |x| { f(x); } }

pub fn main() {
    let v = ~[1, 2, 3, 4, 5];
    let mut sum = 0;
    iter_vec(copy v, |i| {
        iter_vec(copy v, |j| {
            log(error, *i * *j);
            sum += *i * *j;
        });
    });
    log(error, sum);
    assert (sum == 225);
}
