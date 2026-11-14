//! Companies whose own MCP servers and plugins the Marketplace calls official:
//! the GitHub organizations they publish code from and the domains the MCP
//! Registry verified for them. A server or plugin is official when it comes
//! from one of these, not merely when it names the product.

/// (company, GitHub organizations, domains).
pub const PUBLISHERS: &[(&str, &[&str], &[&str])] = &[
    ("Anthropic", &["anthropics"], &["anthropic.com", "claude.ai"]),
    ("Adobe", &["adobe"], &["adobe.com"]),
    ("Algolia", &["algolia"], &["algolia.com"]),
    ("Apify", &["apify"], &["apify.com"]),
    ("Asana", &["asana"], &["asana.com"]),
    ("Atlassian", &["atlassian", "atlassian-labs"], &["atlassian.com"]),
    ("Auth0", &["auth0", "auth0-lab"], &["auth0.com"]),
    ("AWS", &["awslabs", "aws", "aws-samples"], &["aws.amazon.com", "amazon.com"]),
    ("Box", &["box", "box-community"], &["box.com"]),
    ("Brave", &["brave"], &["brave.com"]),
    ("Browserbase", &["browserbase"], &["browserbase.com"]),
    ("Canva", &["canva", "canva-sdks"], &["canva.com"]),
    ("Chroma", &["chroma-core"], &["trychroma.com"]),
    ("CircleCI", &["circleci-public"], &["circleci.com"]),
    ("ClickHouse", &["clickhouse"], &["clickhouse.com"]),
    ("Cloudflare", &["cloudflare"], &["cloudflare.com"]),
    ("Clerk", &["clerk"], &["clerk.com"]),
    ("Cohere", &["cohere-ai"], &["cohere.com"]),
    ("Contentful", &["contentful"], &["contentful.com"]),
    ("Databricks", &["databricks", "databrickslabs"], &["databricks.com"]),
    ("Datadog", &["datadog", "datadog-labs"], &["datadoghq.com"]),
    ("dbt Labs", &["dbt-labs"], &["getdbt.com"]),
    ("DigitalOcean", &["digitalocean", "digitalocean-labs"], &["digitalocean.com"]),
    ("Docker", &["docker"], &["docker.com"]),
    ("Dropbox", &["dropbox"], &["dropbox.com"]),
    ("Dynatrace", &["dynatrace-oss", "dynatrace"], &["dynatrace.com"]),
    ("Elastic", &["elastic"], &["elastic.co"]),
    ("ElevenLabs", &["elevenlabs"], &["elevenlabs.io"]),
    ("Exa", &["exa-labs"], &["exa.ai"]),
    ("Figma", &["figma"], &["figma.com"]),
    ("Firebase", &["firebase"], &["firebase.google.com"]),
    ("Firecrawl", &["mendableai", "firecrawl"], &["firecrawl.dev"]),
    ("GitHub", &["github"], &["github.com"]),
    ("GitLab", &["gitlab-org"], &["gitlab.com"]),
    ("Google", &["google", "googleapis", "googlecloudplatform", "google-gemini", "googlemaps", "chromedevtools", "firebase"], &["google.com", "googleapis.com", "cloud.google.com"]),
    ("Grafana", &["grafana"], &["grafana.com"]),
    ("HashiCorp", &["hashicorp"], &["hashicorp.com"]),
    ("Heroku", &["heroku"], &["heroku.com"]),
    ("HubSpot", &["hubspot"], &["hubspot.com"]),
    ("Hugging Face", &["huggingface"], &["huggingface.co"]),
    ("Intercom", &["intercom"], &["intercom.com"]),
    ("JetBrains", &["jetbrains"], &["jetbrains.com"]),
    ("JFrog", &["jfrog"], &["jfrog.com"]),
    ("LaunchDarkly", &["launchdarkly"], &["launchdarkly.com"]),
    ("Linear", &["linear"], &["linear.app"]),
    ("Mapbox", &["mapbox"], &["mapbox.com"]),
    ("Meilisearch", &["meilisearch"], &["meilisearch.com"]),
    ("Microsoft", &["microsoft", "azure", "azure-samples", "microsoftdocs"], &["microsoft.com", "azure.com"]),
    ("Mistral", &["mistralai"], &["mistral.ai"]),
    ("monday.com", &["mondaycom"], &["monday.com"]),
    ("MongoDB", &["mongodb", "mongodb-js", "mongodb-labs"], &["mongodb.com"]),
    ("Neo4j", &["neo4j", "neo4j-contrib"], &["neo4j.com"]),
    ("Neon", &["neondatabase", "neondatabase-labs"], &["neon.tech", "neon.com"]),
    ("Netlify", &["netlify"], &["netlify.com"]),
    ("New Relic", &["newrelic"], &["newrelic.com"]),
    ("Notion", &["makenotion"], &["notion.com", "notion.so"]),
    ("Okta", &["okta"], &["okta.com"]),
    ("OpenAI", &["openai"], &["openai.com"]),
    ("PagerDuty", &["pagerduty"], &["pagerduty.com"]),
    ("PayPal", &["paypal"], &["paypal.com"]),
    ("Perplexity", &["ppl-ai", "perplexityai"], &["perplexity.ai"]),
    ("Pinecone", &["pinecone-io"], &["pinecone.io"]),
    ("Plaid", &["plaid"], &["plaid.com"]),
    ("PlanetScale", &["planetscale"], &["planetscale.com"]),
    ("Postman", &["postmanlabs"], &["postman.com"]),
    ("PostHog", &["posthog"], &["posthog.com"]),
    ("Prisma", &["prisma"], &["prisma.io"]),
    ("Qdrant", &["qdrant"], &["qdrant.tech"]),
    ("Redis", &["redis"], &["redis.io"]),
    ("Render", &["render-oss"], &["render.com"]),
    ("Salesforce", &["salesforce", "salesforcecli", "forcedotcom"], &["salesforce.com"]),
    ("Sanity", &["sanity-io"], &["sanity.io"]),
    ("SAP", &["sap", "sap-samples"], &["sap.com"]),
    ("Semgrep", &["semgrep"], &["semgrep.dev"]),
    ("Sentry", &["getsentry"], &["sentry.io", "sentry.dev"]),
    ("Shopify", &["shopify"], &["shopify.com", "shopify.dev"]),
    ("Slack", &["slackapi"], &["slack.com"]),
    ("Snowflake", &["snowflake-labs", "snowflakedb"], &["snowflake.com"]),
    ("Snyk", &["snyk"], &["snyk.io"]),
    ("SonarSource", &["sonarsource"], &["sonarsource.com", "sonarqube.com"]),
    ("Square", &["square"], &["squareup.com"]),
    ("Stripe", &["stripe"], &["stripe.com"]),
    // Supabase publishes its MCP server and plugin from supabase-community.
    ("Supabase", &["supabase", "supabase-community"], &["supabase.com"]),
    ("Tavily", &["tavily-ai"], &["tavily.com"]),
    ("Turso", &["tursodatabase"], &["turso.tech"]),
    ("Twilio", &["twilio", "twilio-labs"], &["twilio.com"]),
    ("Upstash", &["upstash"], &["upstash.com", "context7.com"]),
    ("Vercel", &["vercel", "vercel-labs"], &["vercel.com"]),
    ("Webflow", &["webflow"], &["webflow.com"]),
    ("Weaviate", &["weaviate"], &["weaviate.io"]),
    ("Wix", &["wix", "wix-incubator"], &["wix.com"]),
    ("Zapier", &["zapier"], &["zapier.com"]),
    ("Zendesk", &["zendesk"], &["zendesk.com"]),
];

/// The company behind a GitHub organization.
pub fn by_org(org: &str) -> Option<&'static str> {
    let org = org.to_lowercase();
    PUBLISHERS.iter().find(|(_, orgs, _)| orgs.contains(&org.as_str())).map(|(n, _, _)| *n)
}

/// The company a domain (or a subdomain of it: `mcp.figma.com`) belongs to.
pub fn by_domain(domain: &str) -> Option<&'static str> {
    let d = domain.to_lowercase();
    PUBLISHERS.iter().find(|(_, _, ds)| ds.iter().any(|k| d == *k || d.ends_with(&format!(".{k}")))).map(|(n, _, _)| *n)
}

/// The company by its name as a plugin's author writes it (`Google LLC`, `SAP SE`).
pub fn by_name(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    PUBLISHERS.iter().find(|(p, _, _)| {
        let p = p.to_lowercase();
        n == p || n.starts_with(&format!("{p} ")) || n.starts_with(&format!("{p},"))
    }).map(|(n, _, _)| *n)
}

/// Who published an MCP Registry entry, by its namespace: `io.github.<org>/…`
/// (GitHub-verified) or a reverse domain (`com.figma.mcp/…`, DNS-verified).
pub fn by_namespace(server_name: &str) -> Option<&'static str> {
    let ns = server_name.split('/').next().unwrap_or(server_name).to_lowercase();
    if let Some(org) = ns.strip_prefix("io.github.") {
        return by_org(org);
    }
    let domain: Vec<&str> = ns.split('.').rev().collect();
    let domain = domain.join(".");
    // Apps hosted on a platform are their owners', not the platform's.
    if ["execute-api", "up.railway", "vercel.app", "herokuapp", "netlify.app", "workers.dev", "pages.dev", "github.io", "run.app", "azurewebsites"]
        .iter()
        .any(|h| domain.contains(h))
    {
        return None;
    }
    by_domain(&domain)
}

/// The registry searches to run to find every official server: each org's
/// `io.github.<org>/` and each domain reversed (`com.stripe`).
pub fn namespaces() -> Vec<String> {
    let mut out = Vec::new();
    for (_, orgs, domains) in PUBLISHERS {
        out.extend(orgs.iter().map(|o| format!("io.github.{o}/")));
        out.extend(domains.iter().map(|d| d.split('.').rev().collect::<Vec<_>>().join(".")));
    }
    out
}

/// `https://github.com/stripe/ai.git` → `stripe`.
pub fn github_owner(url: &str) -> Option<&str> {
    let rest = url.split("github.com/").nth(1)?;
    rest.split('/').next().filter(|o| !o.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_publishers() {
        assert_eq!(by_namespace("com.figma.mcp/mcp"), Some("Figma"));
        assert_eq!(by_namespace("io.github.github/github-mcp-server"), Some("GitHub"));
        assert_eq!(by_namespace("io.github.PostHog/mcp"), Some("PostHog"));
        assert_eq!(by_namespace("com.googleapis.compute/mcp"), Some("Google"));
        assert_eq!(by_namespace("com.amazonaws.us-east-1.execute-api.x/rules"), None);
        assert_eq!(by_namespace("app.railway.up.x-production/thing"), None);
        assert_eq!(by_namespace("com.robinsonaisystems/github"), None);
        assert_eq!(by_namespace("io.github.someone/stripe-mcp"), None);
        assert_eq!(by_name("Google LLC"), Some("Google"));
        assert_eq!(by_name("SAP SE"), Some("SAP"));
        assert_eq!(by_name("Grafanatic"), None);
        assert_eq!(github_owner("https://github.com/stripe/ai.git"), Some("stripe"));
        assert!(namespaces().contains(&"com.stripe".to_string()));
    }
}
