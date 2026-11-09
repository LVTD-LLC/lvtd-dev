/* Swagger UI on /api/v1/docs. Pinned bundle from the page, no JavaScript build step. */
(function () {
  "use strict";
  var script = document.currentScript;
  if (!script || !window.SwaggerUIBundle) return;
  window.SwaggerUIBundle({
    url: script.dataset.spec,
    dom_id: "#swagger-ui",
    deepLinking: true,
    // The key stays in memory for this tab only; never write it to browser storage.
    persistAuthorization: false,
  });
})();
