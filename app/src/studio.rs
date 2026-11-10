//! Public content carried over from lvtd.dev; styled by the ShipRust layout.
use maud::{Markup, PreEscaped, html};
pub struct SitePage {
    pub path: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub body: &'static str,
}
pub const PAGES: &[SitePage] = &[
    SitePage {
        path: "/projects/",
        title: "Projects.",
        description: "Products, developer tools, and websites built by LVTD. Browse active projects, client work, and the archive, with website and source links.",
        body: include_str!("../content/site/projects.html"),
    },
    SitePage {
        path: "/research/",
        title: "Research.",
        description: "LVTD research and experiments: explore the Jev AI model benchmark and practical AI steering guidance, with results and source code.",
        body: include_str!("../content/site/research.html"),
    },
    SitePage {
        path: "/ai-steering",
        title: "Give your coding agent the context it needs.",
        description: "Choose the right repository context for coding agents. Explore 11 steering file patterns, practical guidance, references, examples, and an installable skill.",
        body: include_str!("../content/site/ai-steering.html"),
    },
    SitePage {
        path: "/jev-benchmark",
        title: "Jev Benchmark",
        description: "An open experiment: Jev judges AI answers head to head across writing, programming, mathematics, and personal advice. Explore the rankings, prompts, and individual decisions.",
        body: include_str!("../content/site/jev-benchmark.html"),
    },
    SitePage {
        path: "/services/hosted-openclaw/",
        title: "Private OpenClaw, deployed and operated for your workflows.",
        description: "LVTD deploys, monitors, maintains, and integrates private OpenClaw instances for teams that want useful AI workflows without the infrastructure burden.",
        body: include_str!("../content/site/services__hosted-openclaw.html"),
    },
    SitePage {
        path: "/terms",
        title: "Terms of Service",
        description: "Terms for using the LVTD website, requesting services, purchasing deposits, and working with LVTD, LLC.",
        body: include_str!("../content/site/tos.html"),
    },
    SitePage {
        path: "/privacy",
        title: "Privacy Policy",
        description: "Privacy policy for LVTD, including website analytics, service inquiries, payments, email, and project data handling.",
        body: include_str!("../content/site/privacy.html"),
    },
    SitePage {
        path: "/jev-benchmark/coding-offline-dynamic-connectivity",
        title: "Offline dynamic connectivity with rollback",
        description: "Compare model answers to Offline dynamic connectivity with rollback. See the prompt, scoring rubric, Elo ranking, and Jev's head-to-head decisions.",
        body: include_str!("../content/site/jev-benchmark__coding-offline-dynamic-connectivity.html"),
    },
    SitePage {
        path: "/jev-benchmark/incident-evidence-synthesis-v1",
        title: "Turn conflicting incident notes into a reliable brief",
        description: "Compare model answers to Turn conflicting incident notes into a reliable brief. See the prompt, scoring rubric, Elo ranking, and Jev's head-to-head decisions.",
        body: include_str!("../content/site/jev-benchmark__incident-evidence-synthesis-v1.html"),
    },
    SitePage {
        path: "/jev-benchmark/math-stopping-time-and-variance",
        title: "A stopping-time expectation and variance",
        description: "Compare model answers to A stopping-time expectation and variance. See the prompt, scoring rubric, Elo ranking, and Jev's head-to-head decisions.",
        body: include_str!("../content/site/jev-benchmark__math-stopping-time-and-variance.html"),
    },
    SitePage {
        path: "/jev-benchmark/personal-friendship-compassion-and-boundaries",
        title: "Being a good friend without abandoning your boundaries",
        description: "Compare model answers to Being a good friend without abandoning your boundaries. See the prompt, scoring rubric, Elo ranking, and Jev's head-to-head decisions.",
        body: include_str!("../content/site/jev-benchmark__personal-friendship-compassion-and-boundaries.html"),
    },
    SitePage {
        path: "/jev-benchmark/support-rollout-analysis-v1",
        title: "Read a rollout experiment without fooling yourself",
        description: "Compare model answers to Read a rollout experiment without fooling yourself. See the prompt, scoring rubric, Elo ranking, and Jev's head-to-head decisions.",
        body: include_str!("../content/site/jev-benchmark__support-rollout-analysis-v1.html"),
    },
    SitePage {
        path: "/jev-benchmark/writing-the-case-for-a-modular-monolith",
        title: "The case for a modular monolith",
        description: "Compare model answers to The case for a modular monolith. See the prompt, scoring rubric, Elo ranking, and Jev's head-to-head decisions.",
        body: include_str!("../content/site/jev-benchmark__writing-the-case-for-a-modular-monolith.html"),
    },
];
pub fn body(page: &SitePage) -> Markup {
    html! { section class="page legal" { h1 { (page.title) } div class="prose" { (PreEscaped(page.body)) } } }
}
