# Northflank Complete Reference

## Services

Services are the primary deployment unit. Three types exist:

### Deployment Service
Deploys pre-built container images from registries (Docker Hub, GHCR, etc.).

```json
{
  "kind": "DeploymentService",
  "spec": {
    "name": "my-service",
    "projectId": "my-project",
    "billing": {
      "deploymentPlan": "nf-compute-200"
    },
    "deployment": {
      "type": "deployment",
      "instances": 1,
      "external": {
        "imagePath": "ghcr.io/org/image:tag"
      }
    },
    "ports": [
      {
        "name": "http",
        "internalPort": 8000,
        "protocol": "HTTP",
        "public": false
      }
    ]
  }
}
```

### Combined Service
Builds from Git and deploys automatically. Best for applications where you want CI/CD in one resource.

### Build Service
Creates builds without deploying. Use with Action nodes to trigger builds manually or deploy elsewhere.

### Service Configuration

**Ports**: Define internal ports with protocols (HTTP, TCP, UDP). Set `public: true` for external access.

**Health Checks**:
```json
"healthChecks": [
  {
    "type": "readinessProbe",
    "protocol": "HTTP",
    "path": "/health",
    "port": 8000,
    "initialDelaySeconds": 30,
    "periodSeconds": 10,
    "timeoutSeconds": 5,
    "failureThreshold": 3
  },
  {
    "type": "livenessProbe",
    "protocol": "HTTP",
    "path": "/health",
    "port": 8000,
    "initialDelaySeconds": 30,
    "periodSeconds": 30,
    "timeoutSeconds": 5,
    "failureThreshold": 3
  }
]
```

**Runtime Environment**: Key-value pairs for environment variables:
```json
"runtimeEnvironment": {
  "DATABASE_URL": "${secrets.DATABASE_URL}",
  "LOG_LEVEL": "info"
}
```

## Compute Plans

### Standard Plans (nf-compute-*)

| Plan | vCPU | Memory | $/hour |
|------|------|--------|--------|
| nf-compute-10 | 0.1 shared | 256 MB | $0.0038 |
| nf-compute-20 | 0.2 shared | 512 MB | $0.0075 |
| nf-compute-50 | 0.5 shared | 1024 MB | $0.0167 |
| nf-compute-100-1 | 1 dedicated | 1024 MB | $0.0250 |
| nf-compute-100-2 | 1 dedicated | 2048 MB | $0.0333 |
| nf-compute-200 | 2 dedicated | 4096 MB | $0.0667 |
| nf-compute-200-8 | 2 dedicated | 8192 MB | $0.1000 |
| nf-compute-400 | 4 dedicated | 8192 MB | $0.1333 |
| nf-compute-800-16 | 8 dedicated | 16384 MB | $0.2667 |
| nf-compute-800-32 | 8 dedicated | 32768 MB | $0.4000 |
| nf-compute-1600-32 | 16 dedicated | 32768 MB | $0.5333 |

Plan naming: `nf-compute-{cpu*100}-{memoryGB}` (memory suffix optional for defaults).

### GPU Plans (nf-gpu-*)

| GPU | $/hour |
|-----|--------|
| NVIDIA L4 24GB | ~$0.80 |
| NVIDIA A100 40GB | $1.42 |
| NVIDIA A100 80GB | $1.76 |
| NVIDIA H100 80GB | $2.74 |
| NVIDIA H200 | $3.14 |
| NVIDIA B200 | $5.87 |

Plan naming pattern: `nf-gpu-{model}-{vram}-{count}g`
- `nf-gpu-a100-80-1g` = 1x A100 80GB
- `nf-gpu-a100-80-2g` = 2x A100 80GB
- `nf-gpu-h100-80-1g` = 1x H100 80GB
- `nf-gpu-l4-24-1g` = 1x L4 24GB

**GPU Configuration in Services**:
```json
{
  "billing": {
    "deploymentPlan": "nf-gpu-a100-80-1g"
  },
  "deployment": {
    "storage": {
      "ephemeralStorage": { "storageSize": 256000 },
      "shmSize": 174080
    },
    "gpu": {
      "enabled": true,
      "configuration": {
        "gpuType": "a100-80",
        "gpuCount": 1
      }
    }
  }
}
```

**GPU Constraints**:
- GPU plans include fixed CPU/memory allocations (not customizable separately)
- Ephemeral storage often needed for model weights (up to 512GB)
- Large `shmSize` required for PyTorch multi-processing (e.g., 170GB for Triton)
- No timeslicing support (one pod per GPU)
- Must create GPU-enabled project in supported region first

## Volumes

Persistent volumes for data that survives container restarts. Volumes can be moved between services.

### Access Modes

| Mode | Description | Scaling |
|------|-------------|---------|
| **RWO** (ReadWriteOnce) | Single instance read/write | Limits service to 1 instance |
| **RWX** (ReadWriteMany) | Multiple instances read/write | Allows horizontal scaling |

**RWX volumes** are a key Northflank differentiator vs other PaaS platforms. They enable parallel workloads sharing common storage - essential for CPU-intensive tasks like transcoding that need to scale horizontally while accessing shared data.

### Configuration

Each volume requires:
- **Container mount path**: Absolute path where volume appears in container (e.g., `/data`)
- **Volume mount path** (optional): Relative path to mount specific subdirectory

### Template Node

Volume node for Infrastructure-as-Code templates:

```json
{
  "kind": "Volume",
  "ref": "my-volume",
  "spec": {
    "name": "my-volume",
    "projectId": "my-project",
    "spec": {
      "storageSize": 153600,
      "accessMode": "ReadWriteMany",
      "storageClassName": "ssd"
    },
    "mounts": [
      {
        "containerMountPath": "/root/.cache/huggingface",
        "volumeMountPath": ""
      }
    ],
    "attachedObjects": [
      {
        "id": "${refs.my-service.id}",
        "type": "service"
      }
    ]
  }
}
```

**accessMode values**: `ReadWriteOnce` (RWO) or `ReadWriteMany` (RWX)

### Constraints

- **Cannot scale down**: Volume storage can only be increased, never decreased
- **RWO limits instances**: Standard volumes restrict service to 1 instance
- **Detach before moving**: Must detach from current service before attaching to another
- **Restart behavior**: Container always terminates before new one starts (regardless of health checks)

### Permissions

Volume ownership transfers to the group specified in your Docker image. If using non-root users:
- Mount only to specific paths needing persistence
- Use `chown` in startup scripts if needed
- Northflank auto-adjusts file group ownership to match container's main process

### CLI Commands

```bash
# Create volume
northflank create volume --file volume.json --projectId my-project

# List volumes in project
northflank list volumes --projectId my-project

# Get volume details
northflank get volume --volumeId my-volume --projectId my-project

# Attach volume to service (via JSON config)
northflank attach volume --volumeId my-volume --projectId my-project --file attach-config.json

# Detach volume (required before moving or deleting)
northflank detach volume --volumeId my-volume --projectId my-project

# Delete volume (must be detached first)
northflank delete volume --volumeId my-volume --projectId my-project
```

### Data Transfer

Transfer files to/from volumes on running services:
```bash
# Using curl/wget from within container
northflank command-exec service --projectId my-project --serviceId my-service -- \
    curl -o /data/file.tar.gz https://example.com/file.tar.gz

# For bulk transfers, deploy an rsync sidecar and attach the volume
```

## Templates

Templates are Infrastructure-as-Code for Northflank. They define workflows that create/update resources declaratively.

### Structure

```json
{
  "apiVersion": "v1",
  "name": "my-template",
  "description": "Template description",
  "arguments": {
    "imageTag": "latest",
    "gpuPlan": "nf-gpu-a100-80-1g"
  },
  "spec": {
    "kind": "Workflow",
    "spec": {
      "type": "sequential",
      "steps": [...]
    }
  }
}
```

### Workflow Types

- **sequential**: Steps execute in order
- **parallel**: Steps execute concurrently

### Node Types

| Kind | Purpose |
|------|---------|
| `Project` | Create projects |
| `DeploymentService` | Deploy container images |
| `BuildService` | Build from Git |
| `CombinedService` | Build + deploy |
| `SecretGroup` | Manage secrets |
| `Addon` | Deploy databases (Postgres, Redis, etc.) |
| `Condition` | Wait for resource state |
| `Action` | Trigger operations (restart, backup, exec) |

### Dynamic Values

**Arguments** - Template parameters:
```json
"imagePath": "ghcr.io/org/image:${args.imageTag}"
```

**References** - Access outputs from earlier nodes:
```json
"serviceId": "${refs.myservice.id}"
"dnsName": "${refs.myservice.ports.0.dns}"
```

**Secrets** - Access secret group values:
```json
"HF_TOKEN": "${secrets.HF_TOKEN}"
```

**Functions** - Built-in utilities:
```json
"${fn.randomSecret(32)}"           // Generate random secret
"${fn.toBase64('value')}"          // Encode base64
"${fn.if(args.dev, 'dev', 'prod')}" // Conditional
"${fn.slug(args.name)}"            // URL-safe slug
```

Many more functions available (string manipulation, math, JSON, arrays) - see [Dynamic Templates docs](https://northflank.com/docs/v1/application/infrastructure-as-code/make-a-template-dynamic).

### Template with Actions

```json
{
  "spec": {
    "kind": "Workflow",
    "spec": {
      "type": "sequential",
      "steps": [
        {
          "kind": "DeploymentService",
          "ref": "triton",
          "spec": { ... }
        },
        {
          "kind": "Action",
          "ref": "restart",
          "spec": {
            "kind": "Service",
            "spec": {
              "type": "restart",
              "data": {
                "projectId": "my-project",
                "serviceId": "${refs.triton.id}"
              }
            }
          }
        }
      ]
    }
  }
}
```

## Secrets

### Secret Groups

Secret groups hold environment variables shared across services. Create them in the UI or via templates/API.

**Linking to Services**: Services can inherit variables from secret groups. Direct service variables override inherited ones.

### Referencing Secrets

In service `runtimeEnvironment`:
```json
"runtimeEnvironment": {
  "API_KEY": "${secrets.API_KEY}"
}
```

In templates, use `argumentOverrides` for sensitive values (stored separately from template body):
```json
{
  "arguments": {
    "apiKey": ""
  },
  "argumentOverrides": {
    "apiKey": "actual-secret-value"
  }
}
```

### Dynamic Templating

Combine variables:
```json
"DATABASE_URL": "postgres://${DB_USER}:${DB_PASS}@${DB_HOST}:5432/db"
```

### Northflank-Injected Variables

Available automatically in all containers:
- `NF_GIT_SHA` - Build commit hash
- `NF_GIT_BRANCH` - Build branch
- `NF_HOSTS` - Comma-separated DNS for public ports
- `NF_PROJECT_ID` - Project identifier
- `NF_OBJECT_ID` - Deployment identifier

## CLI Commands

### Installation

```bash
npm i -g @northflank/cli
# or
yarn global add @northflank/cli
# or without install
npx @northflank/cli
```

### Authentication

```bash
northflank login
# Creates API token at: Account Settings > API > Tokens
```

### Logs

View and stream logs from services, jobs, and addons.

```bash
# Stream service logs in real-time (like tail -f)
northflank get service logs --projectId my-project --serviceId my-service -f

# Get last N lines
northflank get service logs --projectId my-project --serviceId my-service -l 100

# Stream with text filter
northflank get service logs --projectId my-project --serviceId my-service -f --textIncludes "error"

# Stream with regex filter
northflank get service logs --projectId my-project --serviceId my-service -f --regexIncludes "ERROR|WARN"

# Exclude lines matching text
northflank get service logs --projectId my-project --serviceId my-service -f --textNotIncludes "health"

# Logs from specific container (otherwise all containers)
northflank get service logs --projectId my-project --serviceId my-service -f --containerId container-id

# Time range (ISO 8601 or unix timestamp)
northflank get service logs --projectId my-project --serviceId my-service \
    --startTime 2024-01-01T00:00:00Z --endTime 2024-01-02T00:00:00Z

# Duration from start time (seconds)
northflank get service logs --projectId my-project --serviceId my-service \
    --startTime 2024-01-01T00:00:00Z --duration 3600

# Job logs
northflank get job logs --projectId my-project --jobId my-job -f

# Job logs for specific run
northflank get job logs --projectId my-project --jobId my-job --runId RUN_UUID -f

# Addon logs (databases, etc.)
northflank get addon logs --projectId my-project --addonId my-postgres -f
```

**Log Command Flags**:

| Flag | Description |
|------|-------------|
| `-f`, `--tail` | Stream logs in real-time (keeps session open) |
| `-l`, `--lineLimit` | Number of lines to return |
| `--startTime` | Logs after this time (ISO 8601 or unix ts) |
| `--endTime` | Logs before this time |
| `--duration` | Timespan in seconds (use with startTime) |
| `--containerId` | Specific container (default: all) |
| `--textIncludes` | Include lines containing text |
| `--textNotIncludes` | Exclude lines containing text |
| `--regexIncludes` | Include lines matching regex |
| `--regexNotIncludes` | Exclude lines matching regex |
| `-d`, `--direction` | Log order (ignored when tailing) |

### Services

```bash
# List services in a project
northflank list services --projectId my-project

# Get service details
northflank get service --projectId my-project --serviceId my-service

# Create deployment service
northflank create deploymentService --file service.json

# Update service
northflank update service --projectId my-project --serviceId my-service --file service.json

# Restart service
northflank restart service --projectId my-project --serviceId my-service

# Scale service
northflank scale service --projectId my-project --serviceId my-service

# Pause/resume
northflank pause service --projectId my-project --serviceId my-service
northflank resume service --projectId my-project --serviceId my-service
```

### Templates

```bash
# List templates
northflank list templates

# Create template from file
northflank create template --file template.json

# Update existing template
northflank update template --templateId my-template --file template.json

# Run template
northflank run template --templateId my-template

# Run with quiet output (useful in scripts)
northflank run template --templateId my-template --quiet

# Run with file input
northflank run template --templateId my-template --file overrides.json

# Get template details
northflank get template --templateId my-template

# List template runs
northflank list template-runs --templateId my-template

# Delete template
northflank delete template --templateId my-template
```

**Upsert Pattern** (create or update):
```bash
northflank update template --templateId my-template --file template.json 2>/dev/null \
    || northflank create template --file template.json
```

### Secrets

```bash
# List secret groups
northflank list secrets --projectId my-project

# Get secret details
northflank get secret --projectId my-project --secretId my-secrets

# Create secret group
northflank create secret --projectId my-project --file secrets.json

# Update secret group
northflank update secret --projectId my-project --secretId my-secrets --file secrets.json

# Link secret group to service
northflank update secret-link --projectId my-project --secretId my-secrets
```

### Port Forwarding

Forward remote services to localhost (useful for development):

```bash
# Forward specific service (may need sudo for low ports)
northflank forward service --projectId my-project --serviceId my-service --localPort 8000 --port 8000

# Forward specific addon (database)
northflank forward addon --projectId my-project --addonId my-postgres

# Forward all services and addons in project
northflank forward all --projectId my-project
```

### Command Execution

```bash
# Execute command in service container
northflank command-exec service --projectId my-project --serviceId my-service -- /bin/sh

# Execute in job
northflank command-exec job --projectId my-project --jobId my-job -- /bin/sh
```

### Context Management

```bash
# List contexts
northflank context ls

# Set default project
northflank context use project my-project

# Set default service
northflank context use service my-service
```

### Output Formatting

Most commands support output formatting:
```bash
northflank get service --projectId my-project --serviceId my-service -o json
northflank get service --projectId my-project --serviceId my-service -o yaml
```

### Interactive Mode

When flags are omitted, CLI prompts interactively:
```bash
northflank create deploymentService
# Prompts for project, name, image, etc.
```

## Common Patterns

### Deploy GPU Service via Template

```bash
#!/bin/bash
set -euo pipefail

PROJECT="my-project"
TEMPLATE_ID="gpu-service"

# Build and push image first...

# Upsert and run template
northflank update template --templateId "$TEMPLATE_ID" --file template.json 2>/dev/null \
    || northflank create template --file template.json

northflank run template --templateId "$TEMPLATE_ID" --quiet

echo "Forward to local:"
echo "  northflank forward service --projectId $PROJECT --serviceId my-gpu-service --localPort 8000 --port 8000"
```

### Template for GPU Service

```json
{
  "name": "gpu-inference",
  "apiVersion": "v1",
  "arguments": {
    "gpuPlan": "nf-gpu-a100-80-1g",
    "gpuType": "a100-80",
    "imageTag": "latest"
  },
  "spec": {
    "kind": "Workflow",
    "spec": {
      "type": "sequential",
      "steps": [
        {
          "kind": "DeploymentService",
          "ref": "inference",
          "spec": {
            "name": "inference",
            "projectId": "my-project",
            "billing": {
              "deploymentPlan": "${args.gpuPlan}"
            },
            "deployment": {
              "type": "deployment",
              "instances": 1,
              "storage": {
                "ephemeralStorage": { "storageSize": 256000 },
                "shmSize": 65536
              },
              "external": {
                "imagePath": "ghcr.io/org/inference:${args.imageTag}"
              },
              "gpu": {
                "enabled": true,
                "configuration": {
                  "gpuType": "${args.gpuType}",
                  "gpuCount": 1
                }
              }
            },
            "ports": [
              {
                "name": "http",
                "internalPort": 8000,
                "protocol": "HTTP",
                "public": false
              }
            ],
            "runtimeEnvironment": {
              "MODEL_ID": "meta-llama/Llama-3-8B"
            },
            "healthChecks": [
              {
                "type": "readinessProbe",
                "protocol": "HTTP",
                "path": "/health",
                "port": 8000,
                "initialDelaySeconds": 60,
                "periodSeconds": 10,
                "failureThreshold": 30
              }
            ]
          }
        }
      ]
    }
  }
}
```

## References

**Official Documentation**
- [Northflank Documentation](https://northflank.com/docs/) - Main documentation hub
- [Add a Persistent Volume](https://northflank.com/docs/v1/application/databases-and-persistence/add-a-volume) - Volume configuration and constraints
- [Template Writing Guide](https://northflank.com/docs/v1/application/infrastructure-as-code/write-a-template) - Template structure and node types
- [Make a Template Dynamic](https://northflank.com/docs/v1/application/infrastructure-as-code/make-a-template-dynamic) - Args, refs, secrets, and functions syntax
- [Inject Secrets](https://northflank.com/docs/v1/application/secure/inject-secrets) - Secret management and templating
- [Deploy GPUs on Northflank Cloud](https://northflank.com/docs/v1/application/gpu-workloads/deploy-gpus-on-northflank-cloud) - GPU project setup and configuration
- [Configure and Optimise Workloads for GPUs](https://northflank.com/docs/v1/application/gpu-workloads/configure-and-optimise-workloads-for-gpus) - Ephemeral storage, shmSize, CUDA versions

**API Reference**
- [Create Deployment Service API](https://northflank.com/docs/v1/api/services/create-deployment-service) - Service JSON schema including GPU configuration

**CLI Reference**
- [Northflank CLI (npm)](https://www.npmjs.com/package/@northflank/cli) - Installation and usage
- [northflank get service logs](https://fig.io/manual/northflank/get/service/logs) - Log streaming flags and filters
- [northflank get job logs](https://fig.io/manual/northflank/get/job/logs) - Job log flags including --runId
- [northflank attach volume](https://fig.io/manual/northflank/attach/volume) - Volume attachment flags
- [northflank create volume](https://fig.io/manual/northflank/create/volume) - Volume creation flags

**Pricing**
- [Northflank Pricing](https://northflank.com/pricing) - Compute and GPU plan pricing calculator

**Case Studies**
- [How Catalog Built a Scalable Streaming Music Platform](https://northflank.com/blog/case-study-how-catalog-built-a-scalable-streaming-music-platform-with-northflank-cloud-platform-idp) - RWX volumes for horizontal scaling
