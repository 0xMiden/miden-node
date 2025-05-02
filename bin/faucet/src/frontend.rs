use std::collections::HashMap;

use axum::{Json, body::Body, extract::State, response::Response};
use http::{StatusCode, header::CONTENT_TYPE};
use http_body_util::Full;
use static_files::Resource;

use crate::{client::FaucetId, types::AssetOptions};

/// The static website files embedded by the build.rs script.
mod static_resources {
    include!(concat!(env!("OUT_DIR"), "/generated.rs"));
}

/// Holds the static files required by the faucet website.
///
/// aka holds and serves the website files.
pub struct StaticResources {
    css: Resource,
    html: Resource,
    js: Resource,
    favicon: Resource,
    background: Resource,
    metadata: Json<Metadata>,
}

/// Describes the faucet metadata.
///
/// More specifically, the faucet's account ID and allowed mint amounts.
#[derive(Clone, serde::Serialize)]
struct Metadata {
    pub id: FaucetId,
    pub asset_amount_options: AssetOptions,
}

impl StaticResources {
    const HTML_FILENAME: &str = "index.html";
    const CSS_FILENAME: &str = "index.css";
    const JS_FILENAME: &str = "index.js";
    const FAVICON_FILENAME: &str = "favicon.ico";
    const BACKGROUND_FILENAME: &str = "background.png";

    /// Loads the static files required by the frontend website.
    pub fn new(faucet_id: FaucetId, asset_options: AssetOptions) -> Self {
        let metadata = Metadata {
            id: faucet_id,
            asset_amount_options: asset_options,
        };
        let files = static_resources::generate();

        Self {
            css: Self::clone_file(&files, Self::CSS_FILENAME),
            html: Self::clone_file(&files, Self::HTML_FILENAME),
            js: Self::clone_file(&files, Self::JS_FILENAME),
            favicon: Self::clone_file(&files, Self::FAVICON_FILENAME),
            background: Self::clone_file(&files, Self::BACKGROUND_FILENAME),
            metadata: Json(metadata),
        }
    }

    /// Creates a static reference by wrapping itself in a [`Box`] and [leaking](Box::leak) it.
    ///
    /// This is a cheaper alternative to [`Arc`] and is useful if the resource needs to live for
    /// the entire program e.g. such as in this server application.
    pub fn leak(self) -> &'static Self {
        Box::leak(Box::new(self))
    }

    /// Utility to clone a static resource out of a map.
    ///
    /// [`Resource`] does not implement clone, so this is a work-around for that.
    ///
    /// # Panics
    ///
    /// Panics if the requested file does not exist in the resource map.
    fn clone_file(files: &HashMap<&str, Resource>, file: &str) -> Resource {
        // Create a nicer panic message if the file is missing to make debugging easier.
        let Some(file) = files.get(file) else {
            panic!(
                "File {file} not found in the bundled file resources.\n\nAvailable files: {:?}",
                files.keys()
            );
        };

        Resource {
            data: file.data,
            modified: file.modified,
            mime_type: file.mime_type,
        }
    }

    /// Utility which builds an http response from a static file.
    fn build_response(file: &Resource) -> Response {
        // SAFETY: Its a static file resource with a known content type.
        //         This _should_ trivially compose into a valid response.
        //
        //         Ideally we would build each response once and clone it.
        //         But I haven't figured out the magic sauce for this.
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, file.mime_type)
            .body(Body::new(Full::from(file.data)))
            .expect("static file should build into a reponse")
    }
}

pub async fn get_index_html(State(state): State<impl AsRef<StaticResources>>) -> Response {
    StaticResources::build_response(&state.as_ref().html)
}

pub async fn get_index_js(State(state): State<impl AsRef<StaticResources>>) -> Response {
    StaticResources::build_response(&state.as_ref().js)
}

pub async fn get_index_css(State(state): State<impl AsRef<StaticResources>>) -> Response {
    StaticResources::build_response(&state.as_ref().css)
}

pub async fn get_background(State(state): State<impl AsRef<StaticResources>>) -> Response {
    StaticResources::build_response(&state.as_ref().background)
}

pub async fn get_favicon(State(state): State<impl AsRef<StaticResources>>) -> Response {
    StaticResources::build_response(&state.as_ref().favicon)
}

pub async fn get_metadata(State(state): State<impl AsRef<StaticResources>>) -> Json<Metadata> {
    state.as_ref().metadata.clone()
}
