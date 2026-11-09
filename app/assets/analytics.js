// Loaded with defer; the SDK is fetched asynchronously and never blocks the UI.
(function () {
    var config = document.currentScript.dataset;
    var sdk = document.createElement('script');
    sdk.src = config.sdkUrl;
    sdk.async = true;
    sdk.onload = function () {
        if (!window.posthog) return;
        window.posthog.init(config.projectToken, {
            api_host: config.apiHost,
            person_profiles: 'identified_only',
            capture_pageview: false,
            capture_pageleave: config.webAnalytics === 'true',
            autocapture: false,
            capture_exceptions: false,
            capture_performance: config.webAnalytics === 'true' ? { web_vitals: true } : false,
            disable_session_recording: true,
            disable_surveys: true,
            enable_heatmaps: false,
            advanced_disable_feature_flags: true,
            loaded: function (posthog) {
                // Clear a previous account before capturing signed-out visits or
                // switching accounts. Keep anonymous IDs stable between pages.
                var previousUser = posthog.get_property('$user_id');
                if (previousUser && previousUser !== config.userId) posthog.reset(true);
                if (config.userId) posthog.identify(config.userId);
                if (config.webAnalytics === 'true') posthog.capture('$pageview');
            }
        });
    };
    document.head.appendChild(sdk);
})();
