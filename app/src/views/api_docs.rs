//! `/api/v1/docs`: Swagger UI for the generated OpenAPI document. A bare page
//! without the site layout, because Swagger UI brings its own styles.

use crate::AppState;
use maud::{DOCTYPE, Markup, html};

/// Pinned Swagger UI release on jsDelivr. Changing the version means
/// changing both integrity hashes (`openssl dgst -sha384 -binary | openssl base64 -A`).
const SWAGGER_UI: &str = "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.33.0";
const CSS_INTEGRITY: &str = "sha384-Ov4/wv3j2bmct8cDc5X4ngJZohVPzEmc6uDPH8WeljUxO5vtoykvMEfbu9Vh6RaW";
const BUNDLE_INTEGRITY: &str = "sha384-YDALVcy8kj8yltLBVi1vBiBAUqdxvus673gM8XKwiy6aDUJFXivF/KCufekjYbVf";

pub fn swagger(state: &AppState, spec_url: &str) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="robots" content="noindex";
                title { "API reference · " (super::SITE_NAME) }
                link rel="icon" href=(state.asset_url("favicon.svg")) type="image/svg+xml";
                link rel="stylesheet" href={ (SWAGGER_UI) "/swagger-ui.css" } integrity=(CSS_INTEGRITY) crossorigin="anonymous";
            }
            body {
                div id="swagger-ui" {
                    noscript {
                        p { "The interactive reference needs JavaScript. Read the " a href=(spec_url) { "OpenAPI document" } " instead." }
                    }
                }
                script defer src={ (SWAGGER_UI) "/swagger-ui-bundle.js" } integrity=(BUNDLE_INTEGRITY) crossorigin="anonymous" {}
                script defer src=(state.asset_url("api-docs.js")) data-spec=(spec_url) {}
            }
        }
    }
}
