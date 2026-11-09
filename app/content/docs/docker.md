+++
title = "Docker hosting"
slug = "docker"
description = "Deploy the included container with your own hosting setup."
section = "deployment"
order = 1
+++

## Deploy the container

The included `Dockerfile` builds this app as one server binary in a container. Use it with a host that can run Docker images and connect to Postgres.

If your project includes `.github/workflows/deploy.yml` for CapRover, follow the [CapRover guide](/docs/caprover) for that pipeline.

## Configure the app

Provision a Postgres database and set `DATABASE_URL` in your runtime environment. Set `SITE_URL` to your public HTTPS origin; HTTPS enables Secure session cookies. Configure `BIND_ADDR` if your host needs a different listening address.

Keep runtime credentials in your host's environment settings. The app applies its embedded database migrations at startup; the `migrate` command is also available for an explicit migration step.

## Build and deploy

From your project's root:

```sh
docker build -t my-app .
```

Configure your host to run this image with access to your database and route traffic to the app's listening port. The Dockerfile and README describe the image and runtime setup.

Use `/healthz` for a health check. To identify a deployment, pass your commit SHA as the `BUILD_REVISION` Docker build argument and check the `x-deployment-revision` response header after rollout.

## Optional: a separate worker container

The image runs [background jobs](/docs/background-jobs) inside the web process by default. To isolate them, run a second container from the same image with `JOBS_WORKER=only` (or the `worker` command) and the same environment, and set `JOBS_WORKER=false` on the web container. The worker container still answers `/healthz`, so the image's health check applies to both. Give the container at least `JOBS_SHUTDOWN_SECONDS` of stop grace so running jobs can finish.
