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
//! # Tremor http Onramp
//!
//! The `http` onramp process each POST request
//!
//! ## Config
//!
//! * `host` host to listen on (default: "127.0.0.1")
//! * `port` port to listen on (default: 8000)
//! * `tls.cert` tls certificate file to use (default: None)
//! * `tls.port` tls key file to use (default: None)
//!
//! ## Variables
//!
//!
//!
//! ## Endpoints
//!
//! ### PUT `/raw`
//!
//! Takes the PUT body and puts it into the pipeline
//!
//! ### PUT `/v1`
//!
//! Takes the PUT body as a Tremor wrapped message and puts it on the
//! pipeline.
//!
//! #### Content Format
//!
//! Tremor expects a `PUT` request with the `Content-Type` header set to
//! `application-json`. Envelop encoding is, as the content type suggests
//! a JSON.
//!
//! The envelop has the structure:
//!
//! ```json
//! {
//!   "header": {
//!     "uuid": "<base64 encoded raw uuid>",
//!     "parentUuid": "<base64 encoded raw uuid>" | null, // optional
//!     "contentType": "JSON" // other formats may be added.
//!   },
//!   "body": "<base64 encoded raw message>"
//! }
//! ```
//!
//! An example would be:
//! ```json
//! {
//!   "header": {
//!     "contentType":"JSON",
//!     "uuid":"gqB1SNkkEei6oAJCrBEAAg==",
//!     "parentUuid": null
//!   },
//!   "body":"eyJrZXkiOjQyfQ=="
//! }
//! ```

use actix_web::{
    http,
    //error,
    middleware,
    server,
    App,
    AsyncResponder,
    Error,
    HttpMessage,
    HttpRequest,
    HttpResponse,
};
use base64;
use futures::sync::mpsc::channel;
use futures::{Future, Stream};
use onramp::{EnterReturn, Onramp as OnrampT, PipelineOnramp};
use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};
use pipeline::prelude::*;
use serde::{Deserialize, Deserializer};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write;
use std::thread;
use utils;
use uuid::Uuid;

//use std::sync::mpsc::channel;
pub struct Onramp {
    ssl_data: Option<(String, String)>,
    host: String,
    port: u64,
}

impl Onramp {
    pub fn new(opts: &ConfValue) -> Self {
        let ssl_data = match (&opts["tls"]["key"], &opts["tls"]["cert"]) {
            (ConfValue::String(key), ConfValue::String(cert)) => Some((key.clone(), cert.clone())),
            _ => None,
        };
        let port = if let Some(ConfValue::Number(port)) = opts.get("port") {
            port.as_u64().unwrap_or(8000)
        } else {
            8000
        };

        let host = if let Some(ConfValue::String(host)) = opts.get("host") {
            host.clone()
        } else {
            String::from("127.0.0.1")
        };

        Self {
            ssl_data,
            host,
            port,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
enum ContentType {
    #[serde(rename = "UNKNOWN")]
    Unknown,
    #[serde(rename = "JSON")]
    Json,
    #[serde(rename = "MSGPACK")]
    MsgPack,
}

#[derive(Serialize, Deserialize, Debug)]
struct EventHeader {
    #[serde(rename = "contentType")]
    content_type: ContentType,
    #[serde(deserialize_with = "uuid_from_base64")]
    uuid: Uuid,
    #[serde(
        rename = "parentUuid",
        deserialize_with = "perhaps_uuid_from_base64"
    )]
    parent_uuid: Option<Uuid>,
}

pub fn perhaps_uuid_from_base64<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let string: Option<String> = Option::deserialize(deserializer)?;
    match string {
        None => Ok(None),
        Some(string) => {
            let bytes = base64::decode(&string).map_err(|err| Error::custom(err.to_string()))?;
            Uuid::from_slice(&bytes)
                .map_err(|err| Error::custom(err.to_string()))
                .map(Some)
        }
    }
}

pub fn uuid_from_base64<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let string = String::deserialize(deserializer)?;
    let bytes = base64::decode(&string).map_err(|err| Error::custom(err.to_string()))?;
    Uuid::from_slice(&bytes).map_err(|err| Error::custom(err.to_string()))
}

#[derive(Serialize, Deserialize, Debug)]
struct EventWrapper {
    header: EventHeader,
    #[serde(deserialize_with = "vec_u8_from_base64")]
    body: Vec<u8>,
}
pub fn vec_u8_from_base64<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let string = String::deserialize(deserializer)?;
    let bytes = base64::decode(&string).map_err(|err| Error::custom(err.to_string()))?;
    Ok(bytes.to_vec())
}

fn data_v1(req: &HttpRequest<OnrampState>) -> Box<Future<Item = HttpResponse, Error = Error>> {
    let len = req.state().len;
    let i = req.state().idx.get() + 1 % len;
    req.state().idx.set(i);

    let p = req.state().pipeline[i].clone();
    req.json()
        .from_err()
        .and_then(move |event: EventWrapper| {
            let (tx, rx) = channel(0);
            let msg = OnData {
                reply_channel: Some(tx),
                data: EventValue::Raw(event.body),
                vars: HashMap::new(),
                ingest_ns: utils::nanotime(),
            };
            p.do_send(msg);
            rx.take(1)
                .collect()
                .then(|res| match res.unwrap().pop().unwrap().v {
                    Ok(Some(v)) => Ok(HttpResponse::Ok().json(json!({ "ok": format!("{}", v) }))),
                    Ok(None) => {
                        Ok(HttpResponse::Ok().json(json!({ "ok": serde_json::Value::Null })))
                    }
                    Err(_e) => Ok(HttpResponse::new(http::StatusCode::INTERNAL_SERVER_ERROR)),
                })
        }).responder()
}

fn raw(req: &HttpRequest<OnrampState>) -> Box<Future<Item = HttpResponse, Error = Error>> {
    let len = req.state().len;
    let i = req.state().idx.get() + 1 % len;
    req.state().idx.set(i);

    let p = req.state().pipeline[i].clone();
    req.body()
        .from_err()
        .and_then(move |body| {
            let (tx, rx) = channel(0);
            let msg = OnData {
                reply_channel: Some(tx),
                data: EventValue::Raw(body.to_vec()),
                vars: HashMap::new(),
                ingest_ns: utils::nanotime(),
            };
            p.do_send(msg);
            rx.take(1)
                .collect()
                .then(|res| match res.unwrap().pop().unwrap().v {
                    Ok(Some(v)) => Ok(HttpResponse::Ok().json(json!({ "ok": format!("{}", v) }))),
                    Ok(None) => {
                        Ok(HttpResponse::Ok().json(json!({ "ok": serde_json::Value::Null })))
                    }
                    Err(_e) => Ok(HttpResponse::new(http::StatusCode::INTERNAL_SERVER_ERROR)),
                })
        }).responder()
}
struct OnrampState {
    pipeline: PipelineOnramp,
    len: usize,
    idx: Cell<usize>,
}
impl OnrampT for Onramp {
    fn enter_loop(&mut self, pipelines: PipelineOnramp) -> EnterReturn {
        let host = self.host.clone();
        let port = self.port;
        let ssl_data = self.ssl_data.clone();
        thread::spawn(move || {
            let s = server::new(move || {
                App::with_state(OnrampState {
                    pipeline: pipelines.clone(),
                    idx: Cell::new(0),
                    len: pipelines.len(),
                }).middleware(middleware::Logger::default())
                .resource("/v1", |r| r.method(http::Method::PUT).f(data_v1))
                .resource("/raw", |r| r.method(http::Method::PUT).f(raw))
            });
            let host = format!("{}:{}", host, port);
            if let Some((ref key, ref cert)) = ssl_data {
                let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
                builder
                    .set_private_key_file(&key, SslFiletype::PEM)
                    .unwrap();
                builder.set_certificate_chain_file(&cert).unwrap();
                println_stderr!("Binding HTTPs OnRamp to https://{}", host);
                s.bind_ssl(&host, builder)
                    .unwrap_or_else(|_| panic!("Can not bind to {}", host))
                    .run()
            } else {
                println_stderr!("Binding HTTP OnRamp to http://{}", host);
                s.bind(&host)
                    .unwrap_or_else(|_| panic!("Can not bind to {}", host))
                    .run()
            };
        })
    }
}
#[cfg(test)]
mod tests {}
