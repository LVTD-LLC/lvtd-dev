+++
title = "CapRover"
slug = "caprover"
description = "Deploy from GitHub with the generated CapRover workflow."
section = "deployment"
order = 2
+++

## Use the CapRover workflow

This guide applies if your project includes the CapRover workflow at `.github/workflows/deploy.yml`. Check that file before configuring deployment. If it is absent, follow [Docker hosting](/docs/docker) and connect the image to your own deployment pipeline.

The included workflow targets the CapRover app `lvtd-dev`. It builds a Docker image, publishes it to GitHub Container Registry, and deploys on pushes to `main`. Run the checks in `AGENTS.md` before pushing: the deployment workflow does not run tests.

## Set up deployment

1. Create a CapRover app named `lvtd-dev`, matching the workflow. Enable its **App Token** under Deployment.
2. Add your domain, enable HTTPS, and force HTTPS.
3. Add the GitHub repository secrets `CAPROVER_SERVER` (your captain URL) and `APP_TOKEN` (the app token).
4. Give CapRover pull access to GitHub Container Registry if your image package is private.
5. Configure `DATABASE_URL` and `SITE_URL` in the app's runtime environment. Set `SITE_URL` to your public HTTPS origin. Add your [Sentry](/docs/sentry) or [PostHog](/docs/posthog) configuration if enabled.

## Run background jobs in their own app

By default the single app runs [background jobs](/docs/background-jobs)
inside the web process, which is fine for most products. The workflow can
also deploy the same image to a second CapRover app that only runs jobs,
so heavy or crash-prone work never competes with requests and can be scaled
or restarted on its own. Nothing in the workflow changes; it deploys the
worker whenever the `WORKER_APP_TOKEN` secret exists.

1. In CapRover, create an app named like the web app with a `-worker`
   suffix (the workflow computes `${APP}-worker` from its `APP` value). Leave
   **Has persistent data** off and add no domain.
2. Give it the same runtime environment as the web app (`DATABASE_URL`,
   `SITE_URL`, mail settings, Sentry or PostHog if enabled) plus
   `JOBS_WORKER=only`. Leave the instance count at 1.
3. Enable the worker app's **App Token** under Deployment and add it as the
   `WORKER_APP_TOKEN` repository secret.
4. Push to `main`. The workflow deploys the web app, then the worker. In the
   worker's CapRover logs you should see `job worker started` with the
   registered job kinds; the image's health check passes because the worker
   role answers `/healthz`.
5. Only now set `JOBS_WORKER=false` on the web app so it stops claiming jobs
   and running the hourly cleanups. Do it last: a web app set to `false`
   with no running worker means jobs wait in the queue.

Both apps apply migrations at startup, so deploy order does not matter.
During a rollout the old and new containers may run jobs at the same time;
that is safe as long as job kinds are never renamed. To inspect the queue in
production, run `jobs stats` inside the worker container, for example
`docker exec <container> lvtd-dev jobs stats` on the server.

Capacity note: each CapRover app takes one address on the Swarm overlay
network plus one per running container, and a zero-downtime rollout briefly
needs one more. The default network is a `/24`, so a server with around 120
apps runs out. If a deploy sits in the `New` state, check
`journalctl -u docker` on the server for `could not find an available IP`
and remove unused apps before suspecting your build.

## Verify your release

Set the GitHub repository variable `PRODUCTION_URL` to your public origin. This enables the workflow's rollout check: `/healthz` must succeed and the `x-deployment-revision` header must match the pushed commit SHA.

Push your code to `main` and inspect the workflow result. Confirm the deployed app responds on your domain. Keep the README and workflow comments as the reference for your project's deployment setup.
