/* Optional browser telemetry. Pinned SDK, no JavaScript build step. */
(function () {
  "use strict";
  var script = document.currentScript;
  if (!script || !script.dataset.dsn) return;
  var config = script.dataset;
  var errors = config.errors === "true";
  var traces = config.traces === "true";
  var logs = config.logs === "true";
  var metrics = config.metrics === "true";
  var replay = config.replay === "true";
  if (!errors && !traces && !logs && !metrics && !replay) return;
  var sdk = document.createElement("script");
  sdk.async = true;
  sdk.src = "https://browser.sentry-cdn.com/11.4.0/bundle.tracing.replay.logs.metrics.min.js";
  sdk.integrity = "sha384-MBavHU2hESv8bavTAcMxfcWsJjP24CNCDwf4dADuN9s4lFPMthEqo5e85AAPWgxO";
  sdk.crossOrigin = "anonymous";
  sdk.onload = async function () {
    try {
      var Sentry = window.Sentry;
      var integrations = [];
      if (traces) integrations.push(Sentry.browserTracingIntegration({
          beforeStartSpan: function (context) {
            context.name = cleanUrl(context.name);
            return context;
          },
        }));
      if (replay) integrations.push(Sentry.replayIntegration({
          maskAllText: true,
          maskAllInputs: true,
          blockAllMedia: true,
          // Keep network payloads and console content out of recordings.
          networkDetailAllowUrls: [],
          beforeAddRecordingEvent: function (event) {
            if (event.data && event.data.tag === "breadcrumb") return null;
            if (event.type === 4 && event.data && event.data.href) event.data.href = cleanUrl(event.data.href);
            return event;
          },
        }));
      if (traces && typeof window.Profiler === "function" && Number(config.profileRate) > 0) {
        try {
          var profiling = await Sentry.lazyLoadIntegration("browserProfilingIntegration");
          integrations.push(profiling());
        } catch (_) { /* Profiling is optional when CDN access is blocked. */ }
      }
      Sentry.init({
        dsn: config.dsn,
        environment: config.environment,
        release: config.release,
        dataCollection: { userInfo: false, cookies: false, httpHeaders: false, httpBodies: [], urlQueryParams: false },
        defaultIntegrations: errors ? undefined : false,
        integrations: integrations,
        traceLifecycle: "static",
        tracesSampleRate: traces ? Number(config.tracesRate) : 0,
        replaysSessionSampleRate: replay ? Number(config.replayRate) : 0,
        replaysOnErrorSampleRate: replay && errors ? 1.0 : 0,
        profileSessionSampleRate: traces ? Number(config.profileRate) : 0,
        profileLifecycle: "trace",
        beforeSendLog: function (log) { return logs ? log : null; },
        beforeSendMetric: function (metric) { return metrics ? metric : null; },
        // Propagate only to this site; never to PostHog or Sentry collectors.
        tracePropagationTargets: traces ? [new RegExp("^" + window.location.origin.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "(?:/|$)"), /^\/(?!\/)/] : [],
        beforeBreadcrumb: function (breadcrumb) {
          if (breadcrumb.category === "console" || breadcrumb.category === "ui.input") return null;
          if (breadcrumb.data) {
            ["url", "from", "to"].forEach(function (key) {
              if (breadcrumb.data[key]) breadcrumb.data[key] = cleanUrl(breadcrumb.data[key]);
            });
          }
          return breadcrumb;
        },
        beforeSend: function (event) { return errors ? sanitizeEvent(event) : null; },
        beforeSendTransaction: function (event) { return traces ? sanitizeEvent(event) : null; },
        beforeSendSpan: Sentry.withStaticSpan(function (span) {
          if (span.description) span.description = cleanUrl(span.description);
          if (span.data) {
            ["url.full", "url.path", "http.url", "http.target", "sentry.segment.name"].forEach(function (key) {
              if (span.data[key]) span.data[key] = cleanUrl(span.data[key]);
            });
            delete span.data["url.query"];
            delete span.data["url.fragment"];
          }
          return span;
        }),
      });
      // Replay metadata uses event processors rather than beforeSend.
      Sentry.addEventProcessor(function (event) {
        if (event.type === "replay_event" && Array.isArray(event.urls)) {
          event.urls = event.urls.map(cleanUrl);
          return sanitizeEvent(event);
        }
        return event;
      });
      if (logs) Sentry.logger.info("browser telemetry initialized");
      if (metrics) Sentry.metrics.count("browser.page_load", 1);
    } catch (_) { /* Telemetry must never prevent normal site behavior. */ }
  };
  document.head.appendChild(sdk);

  function cleanUrl(value) {
    return String(value).split(/[?#]/)[0].replace(/\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(?=\/|$)/gi, "/:id");
  }

  function sanitizeEvent(event) {
    if (event.request) {
      event.request = { url: cleanUrl(event.request.url || ""), method: event.request.method };
    }
    if (event.transaction) event.transaction = cleanUrl(event.transaction);
    return event;
  }
})();
