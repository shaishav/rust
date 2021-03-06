// Copyright 2012 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[link(name = "static_methods_crate",
       vers = "0.1")];

#[crate_type = "lib"];

pub trait read {
    static fn readMaybe(s: ~str) -> Option<Self>;
}

impl read for int {
    static fn readMaybe(s: ~str) -> Option<int> {
        int::from_str(s)
    }
}

impl read for bool {
    static fn readMaybe(s: ~str) -> Option<bool> {
        match s {
          ~"true" => Some(true),
          ~"false" => Some(false),
          _ => None
        }
    }
}

pub fn read<T:read + Copy>(s: ~str) -> T {
    match read::readMaybe(s) {
      Some(x) => x,
      _ => fail!(~"read failed!")
    }
}
