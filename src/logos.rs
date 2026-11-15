//! Brand marks for the services agents connect to: a tile in the brand's
//! color with its logo (simple-icons, CC0, in assets/logos), or with its
//! initial when there is no logo to show.

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, Hsla, ParentElement as _, Rgba, Styled as _, div, px, rgb, svg};

use crate::theme;
use crate::ui::{self, Type, Typography as _};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    /// `assets/logos/<slug>.svg` on the brand's color.
    Logo { slug: &'static str, hex: u32 },
    /// An initial on a color.
    Letter { ch: char, hex: u32 },
}

/// Brands known by a word in the name or address: (word, logo, color).
/// The logo is empty where simple-icons has none (the brand asked to be left out).
const BRANDS: &[(&str, &str, u32)] = &[
    // Before "claude": claude-seo is not Claude.
    ("seo", "", 0x2563EB),
    ("gmail", "gmail", 0xEA4335),
    ("calendar", "googlecalendar", 0x4285F4),
    ("drive", "googledrive", 0x4285F4),
    ("sheets", "googlesheets", 0x34A853),
    ("slides", "googleslides", 0xFBBC04),
    ("docs", "googledocs", 0x4285F4),
    ("google", "google", 0x4285F4),
    ("anthropic", "claude", 0xD97757),
    ("claude", "claude", 0xD97757),
    ("posthog", "posthog", 0x1D1F27),
    ("supabase", "supabase", 0x3FCF8E),
    ("notion", "notion", 0x000000),
    ("figma", "figma", 0xF24E1E),
    ("linear", "linear", 0x5E6AD2),
    ("asana", "asana", 0xF06A6A),
    ("atlassian", "atlassian", 0x0052CC),
    ("jira", "jira", 0x0052CC),
    ("confluence", "confluence", 0x172B4D),
    ("box", "box", 0x0061D5),
    ("hubspot", "hubspot", 0xFF7A59),
    ("intercom", "intercom", 0x1F8DED),
    ("stripe", "stripe", 0x635BFF),
    ("cloudflare", "cloudflare", 0xF38020),
    ("github", "github", 0x181717),
    ("gitlab", "gitlab", 0xFC6D26),
    ("vercel", "vercel", 0x000000),
    ("sentry", "sentry", 0x362D59),
    ("discord", "discord", 0x5865F2),
    ("docker", "docker", 0x2496ED),
    ("dropbox", "dropbox", 0x0061FF),
    ("airtable", "airtable", 0x18BFFF),
    ("mongodb", "mongodb", 0x47A248),
    ("redis", "redis", 0xFF4438),
    ("postgres", "postgresql", 0x4169E1),
    ("prisma", "prisma", 0x2D3748),
    ("neon", "neon", 0x00E599),
    ("resend", "resend", 0x000000),
    ("clerk", "clerk", 0x6C47FF),
    ("shopify", "shopify", 0x7AB55C),
    ("solana", "solana", 0x9945FF),
    ("trello", "trello", 0x0052CC),
    ("todoist", "todoist", 0xE44332),
    ("zapier", "zapier", 0xFF4F00),
    ("grafana", "grafana", 0xF46800),
    ("datadog", "datadog", 0x632CA6),
    ("typescript", "typescript", 0x3178C6),
    ("rust", "rust", 0x000000),
    ("clangd", "llvm", 0x262D3A),
    ("python", "python", 0x3776AB),
    ("brave", "brave", 0xFB542B),
    ("davinci", "davinciresolve", 0x233A51),
    ("npm", "npm", 0xCB3837),
    ("algolia", "algolia", 0x003DFF),
    ("auth0", "auth0", 0xEB5424),
    ("huggingface", "huggingface", 0xFFD21E),
    ("hugging", "huggingface", 0xFFD21E),
    ("elevenlabs", "elevenlabs", 0x000000),
    ("perplexity", "perplexity", 0x1FB8CD),
    ("mapbox", "mapbox", 0x000000),
    ("snyk", "snyk", 0x4C4A73),
    ("sonar", "sonar", 0xFD3456),
    ("newrelic", "newrelic", 0x1CE783),
    ("pagerduty", "pagerduty", 0x06AC38),
    ("jfrog", "jfrog", 0x40BE46),
    ("hashicorp", "hashicorp", 0x000000),
    ("terraform", "hashicorp", 0x000000),
    ("neo4j", "neo4j", 0x4581C3),
    ("sanity", "sanity", 0x0D0E12),
    ("snowflake", "snowflake", 0x29B5E8),
    ("upstash", "upstash", 0x00E9A3),
    ("webflow", "webflow", 0x146EF5),
    ("wix", "wix", 0x0C6EFC),
    ("paypal", "paypal", 0x002991),
    ("planetscale", "planetscale", 0x000000),
    ("postman", "postman", 0xFF6C37),
    ("firebase", "firebase", 0xDD2C00),
    ("dynatrace", "dynatrace", 0x1496FF),
    ("circleci", "circleci", 0x343434),
    ("sap", "sap", 0x0FAAFF),
    ("elastic", "elastic", 0x005571),
    ("clickhouse", "clickhouse", 0xFFCC01),
    ("okta", "okta", 0x007DC1),
    ("netlify", "netlify", 0x00C7B7),
    ("digitalocean", "digitalocean", 0x0080FF),
    ("jetbrains", "jetbrains", 0x000000),
    ("turso", "turso", 0x4FF8D2),
    ("qdrant", "qdrant", 0xDC244C),
    ("meilisearch", "meilisearch", 0xFF5CAA),
    ("contentful", "contentful", 0x2478CC),
    ("zendesk", "zendesk", 0x03363D),
    ("mistral", "mistralai", 0xFA520F),
    ("databricks", "databricks", 0xFF3621),
    ("aws", "", 0xFF9900),
    ("amazon", "", 0xFF9900),
    ("microsoft", "", 0x0078D4),
    ("azure", "", 0x0078D4),
    ("apify", "", 0x00B4A0),
    ("adobe", "", 0xFA0F00),
    ("twilio", "", 0xF22F46),
    ("exa", "", 0x1F40ED),
    ("tavily", "", 0x4F46E5),
    ("browserbase", "", 0xF03603),
    ("firecrawl", "", 0xFF6B00),
    ("pinecone", "", 0x000000),
    ("slack", "", 0x4A154B),
    ("canva", "", 0x00C4CC),
    ("monday", "", 0xFF3D57),
    ("openai", "", 0x10A37F),
    ("playwright", "", 0x2EAD33),
    ("helius", "", 0xE84125),
    ("context7", "", 0x0E0E10),
];

/// The mark for a server, plugin or skill called `name`, at `hint` (a URL or command).
pub fn mark(name: &str, hint: &str) -> Mark {
    // Every claude.ai connector says "claude"; the rest of the name is the brand.
    let name = name.strip_prefix("claude.ai ").unwrap_or(name);
    // The name decides; the address only when the name says nothing, and not by
    // where it is hosted (a github.com homepage is not GitHub's server).
    let found = brand(name, &[]).or_else(|| {
        brand(
            &format!("{name} {hint}"),
            &["github", "gitlab", "npm", "claude", "anthropic", "vercel"],
        )
    });
    if let Some((slug, hex)) = found {
        return if slug.is_empty() {
            Mark::Letter {
                ch: initial(name),
                hex,
            }
        } else {
            Mark::Logo { slug, hex }
        };
    }
    let hash = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    Mark::Letter {
        ch: initial(name),
        hex: theme::CATEGORY[hash as usize % theme::CATEGORY.len()],
    }
}

/// The first brand a word of `text` names, leaving out the `skip` ones.
fn brand(text: &str, skip: &[&str]) -> Option<(&'static str, u32)> {
    let hay = text.to_lowercase();
    let words: Vec<&str> = hay
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let google = hay.contains("google");
    for &(word, slug, hex) in BRANDS {
        // Google's products only under Google, so "docs" alone stays a plain word.
        let product = matches!(word, "calendar" | "drive" | "sheets" | "slides" | "docs");
        if (product && !google) || skip.contains(&word) {
            continue;
        }
        let hit = match word {
            // Short words only as a whole word.
            "box" | "seo" | "npm" | "rust" | "neon" | "docs" | "sap" | "exa" | "wix" | "aws"
            | "okta" | "sonar" => words.contains(&word),
            _ => words.iter().any(|w| w.starts_with(word)),
        };
        if hit {
            return Some((slug, hex));
        }
    }
    None
}

fn initial(name: &str) -> char {
    let core = name.rsplit(':').next().unwrap_or(name);
    core.chars()
        .find(|c| c.is_alphanumeric())
        .map_or('?', |c| c.to_ascii_uppercase())
}

/// Relative luminance of a color, 0 (black) to 1 (white).
fn luminance(hex: u32) -> f32 {
    let lin = |c: u32| {
        let c = c as f32 / 255.;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(hex >> 16 & 0xff) + 0.7152 * lin(hex >> 8 & 0xff) + 0.0722 * lin(hex & 0xff)
}

/// A rounded tile with the mark, `size` points square.
pub fn tile(mark: Mark, size: f32) -> Div {
    let hex = match mark {
        Mark::Logo { hex, .. } | Mark::Letter { hex, .. } => hex,
    };
    let lum = luminance(hex);
    let ink: Hsla = if lum > 0.45 {
        rgb(0x111111).into()
    } else {
        gpui::white()
    };
    let bg: Rgba = rgb(hex);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(crate::ui::rad(if size >= 28. {
            ui::RADIUS
        } else {
            ui::RADIUS_SM
        }))
        .bg(bg)
        // Near-black marks vanish on the dark page without an edge.
        .when(lum < 0.03, |d| {
            d.border_1().border_color(theme::border_strong())
        })
        .map(|d| match mark {
            Mark::Logo { slug, .. } => d.child(
                svg()
                    .path(format!("logos/{slug}.svg"))
                    .size(px(size * 0.56))
                    .text_color(ink),
            ),
            Mark::Letter { ch, .. } => d.child(
                div()
                    .ty(if size >= 28. {
                        Type::Heading
                    } else {
                        Type::Label
                    })
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(ink)
                    .child(ch.to_string()),
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug(name: &str, hint: &str) -> String {
        match mark(name, hint) {
            Mark::Logo { slug, .. } => slug.to_string(),
            Mark::Letter { ch, .. } => format!("letter {ch}"),
        }
    }

    #[test]
    fn finds_brands() {
        assert_eq!(
            slug("claude.ai Gmail", "https://gmailmcp.googleapis.com/mcp/v1"),
            "gmail"
        );
        assert_eq!(
            slug(
                "claude.ai Google Docs",
                "https://docsmcp.googleapis.com/mcp/v1"
            ),
            "googledocs"
        );
        assert_eq!(
            slug(
                "claude.ai Claude Docs",
                "https://api.anthropic.com/v1/pages/mcp"
            ),
            "claude"
        );
        assert_eq!(slug("posthog", "https://mcp.posthog.com/mcp"), "posthog");
        assert_eq!(slug("plugin:posthog:posthog", ""), "posthog");
        assert_eq!(slug("claude.ai Box", "https://mcp.box.com"), "box");
        assert_eq!(
            slug("claude.ai Slack", "https://mcp.slack.com/mcp"),
            "letter S"
        );
        assert_eq!(slug("rust-analyzer-lsp", ""), "rust");
        assert_eq!(slug("claude-seo", "agricidaniel-claude-seo"), "letter C");
        assert_eq!(slug("helius", "npx helius-mcp@latest"), "letter H");
        assert_eq!(slug("toolbox", "npx something"), "letter T");
        assert_eq!(
            slug("superpowers", "https://github.com/obra/superpowers"),
            "letter S"
        );
        assert_eq!(
            slug(
                "github",
                "https://github.com/anthropics/claude-plugins-official"
            ),
            "github"
        );
        assert!(luminance(0xffffff) > 0.99 && luminance(0) < 0.01);
    }
}
