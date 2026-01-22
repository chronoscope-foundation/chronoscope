---
name: northflank
description: Northflank deployment platform. Use when deploying to Northflank, writing Northflank templates, configuring GPU workloads, or troubleshooting Northflank services and logs.
user-invocable: false
---

# Northflank Platform Guide

Northflank is a developer platform for deploying containers, databases, and GPU workloads.

## Quick Reference

**CLI Install**: `npm i -g @northflank/cli`

## Viewing Logs

```bash
# Stream logs in real-time (like tail -f)
northflank get service logs --projectId PROJECT --serviceId SERVICE -f

# Get last N lines
northflank get service logs --projectId PROJECT --serviceId SERVICE -l 100

# Filter by text
northflank get service logs --projectId PROJECT --serviceId SERVICE -f --textIncludes "error"

# Filter by regex
northflank get service logs --projectId PROJECT --serviceId SERVICE -f --regexIncludes "ERROR|WARN"

# Specific container (otherwise shows all)
northflank get service logs --projectId PROJECT --serviceId SERVICE -f --containerId CONTAINER

# Time range
northflank get service logs --projectId PROJECT --serviceId SERVICE --startTime 2024-01-01T00:00:00Z --endTime 2024-01-02T00:00:00Z

# Job logs (include --runId for specific run)
northflank get job logs --projectId PROJECT --jobId JOB -f
```

Key flags: `-f`/`--tail` (stream), `-l`/`--lineLimit`, `--textIncludes`, `--regexIncludes`, `--startTime`, `--endTime`

## Common Commands

```bash
northflank login                                    # Authenticate
northflank list services --projectId PROJECT        # List services
northflank forward service --projectId PROJECT --serviceId SERVICE --localPort 8000 --port 8000

# Templates (upsert pattern)
northflank update template --templateId ID --file template.json 2>/dev/null \
    || northflank create template --file template.json
northflank run template --templateId ID --quiet
```

**Plan Naming**:
- Compute: `nf-compute-{cpu*100}-{memoryGB}` (e.g., `nf-compute-200`, `nf-compute-400-16`)
- GPU: `nf-gpu-{model}-{vram}-{count}g` (e.g., `nf-gpu-a100-80-1g`, `nf-gpu-h100-80-2g`)

**Template Dynamic Values**:
- Arguments: `${args.imageTag}`
- References: `${refs.service.id}`, `${refs.service.ports.0.dns}`
- Secrets: `${secrets.API_KEY}`
- Functions: `${fn.randomSecret(32)}`, `${fn.if(args.dev, 'a', 'b')}`

## Volumes

```bash
# Create volume
northflank create volume --file volume.json --projectId PROJECT

# List/get volumes
northflank list volumes --projectId PROJECT
northflank get volume --volumeId VOLUME --projectId PROJECT

# Attach/detach (must detach before moving to another service)
northflank attach volume --volumeId VOLUME --projectId PROJECT
northflank detach volume --volumeId VOLUME --projectId PROJECT
```

**Access modes**:
- **RWO (ReadWriteOnce)**: Default. Limits service to 1 instance.
- **RWX (ReadWriteMany)**: Allows multiple instances to read/write. Required for horizontal scaling of stateful workloads.

RWX volumes are a key Northflank differentiator vs other PaaS - enables parallel workloads sharing storage.

For complete documentation including service configuration, template structure, all CLI commands, GPU constraints, and reference links, see [reference.md](reference.md).
