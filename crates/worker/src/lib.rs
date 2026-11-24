//! Cloudflare Worker adapter.
//!
//! The adapter is deliberately dumb: it reconstructs the method and path from
//! the request, hands both to the runtime-independent core, and copies the
//! resulting reply onto the response without interpreting it.

use vagrant_registry_core::http::{Body, Reply, handle, request_path};
use worker::*;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // A bare "/*" is rejected by matchit 0.7, so the catch-all is unnamed-param
    // form. The static root still wins for "/".
    Router::new()
        .on_async("/", handler)
        .on_async("/*path", handler)
        .run(req, env)
        .await
}

async fn handler(req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    // `path()` is already query- and fragment-free with percent-encoding intact;
    // `request_path` keeps the adapter honest if that ever changes.
    let method = req.method();
    let reply = handle(method.as_ref(), request_path(&req.path()));
    respond(reply)
}

fn respond(reply: Reply) -> Result<Response> {
    let response = match reply.body {
        Body::Empty => Response::empty()?,
        Body::Json(bytes) => Response::from_bytes(bytes.to_vec())?,
        Body::Html(document) => Response::from_bytes(document.into_bytes())?,
        Body::Text(text) => Response::from_bytes(text.into_bytes())?,
    };
    let response = response.with_status(reply.status);

    for (name, value) in reply.headers {
        response.headers().set(name, value)?;
    }

    Ok(response)
}
