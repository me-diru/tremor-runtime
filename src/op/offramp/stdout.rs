// Copyright 2018, Wayfair GmbH
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//!
//! # Tremor stdout Offramp
//!
//! The `stdout` offramp writes events to the standard output (conse). It can be configured with:
//!
//! ## Config
//! * `prefix` - string a prepend to a message
//!
//! ## Variables
//!
//! * `prefix` - sets the prefix (overrides the configuration)

use error::TSError;
use errors::*;
use pipeline::prelude::*;
use serde_yaml;
/// An offramp that write to stdout

#[derive(Debug, Clone, Deserialize)]
pub struct Offramp {
    prefix: String,
}

impl Offramp {
    pub fn new(opts: &ConfValue) -> Result<Self> {
        if opts.is_null() {
            Ok(Offramp {
                prefix: String::from(""),
            })
        } else {
            Ok(serde_yaml::from_value(opts.clone())?)
        }
    }
}
impl Opable for Offramp {
    fn exec(&mut self, event: EventData) -> EventResult {
        ensure_type!(event, "offramp::stdout", ValueType::Raw);

        let pfx = if let Some(MetaValue::String(ref pfx)) = event.var(&"prefix") {
            pfx.clone()
        } else {
            self.prefix.clone()
        };

        if let (ret, EventValue::Raw(raw)) = event.make_return_and_value(Ok(None)) {
            match String::from_utf8(raw.to_vec()) {
                Ok(s) => println!("{}{}", pfx, s),
                Err(e) => println!("{}{:?}", pfx, e),
            }
            EventResult::Return(ret)
        } else {
            unreachable!()
        }
    }
    opable_types!(ValueType::Raw, ValueType::Raw);
}
