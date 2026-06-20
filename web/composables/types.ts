// TypeScript types loosely mirroring the Rust models in ocf-core and the
// subsystem crates. These intentionally use the JSON shape the REST API is
// expected to emit (serde defaults: structs -> objects, enums -> tagged or
// string), not the exact Rust generics.

// ── Core (ocf-core) ─────────────────────────────────────────────────────────

export type Id = string

export interface Metadata {
  id: Id
  name: string
  labels?: Record<string, string>
  annotations?: Record<string, string>
  created_at?: string
  updated_at?: string
}

/** ocf-core::health::Health */
export type Health = 'Healthy' | 'Degraded' | 'Unhealthy' | 'Unknown'

/** ocf-core::health::LifecycleState */
export type LifecycleState =
  | 'Pending'
  | 'Creating'
  | 'Running'
  | 'Stopping'
  | 'Stopped'
  | 'Failed'
  | 'Unknown'

/** ocf-core::scope::ScopeLevel */
export type ScopeLevel = 'Fleet' | 'Region' | 'Datacenter' | 'Rack' | 'Machine'

/** ocf-core::quantity::ResourceSpec */
export interface ResourceSpec {
  cpu_millis: number
  memory_bytes: number
  disk_bytes: number
}

// ── Topology (ocf-topology) ─────────────────────────────────────────────────

/**
 * A node in the drill-down tree. The API flattens the
 * fleet → region → datacenter → rack → machine hierarchy into a recursive
 * structure so the frontend can render it generically.
 */
export interface TopologyNode {
  id: Id
  name: string
  level: ScopeLevel
  health: Health
  /** Machine-only details, populated at the leaf. */
  machine?: MachineDetail
  children: TopologyNode[]
}

export interface MachineDetail {
  id: Id
  hostname: string
  health: Health
  state: LifecycleState
  cpu_cores: number
  memory_bytes: number
  disk_bytes: number
  ip_address?: string
  rack?: string
  workload_count: number
  labels?: Record<string, string>
}

// ── Runtime (ocf-runtime) ───────────────────────────────────────────────────

export type RuntimeKind = 'Container' | 'VirtualMachine'

export interface Workload {
  metadata: Metadata
  kind: RuntimeKind
  image: string
  resources: ResourceSpec
  state: LifecycleState
  node?: Id | null
  node_name?: string | null
  highly_available: boolean
  placement?: string | null
  env?: Record<string, string>
}

// ── Network (ocf-network) ───────────────────────────────────────────────────

export interface Vpc {
  metadata: Metadata
  cidr: string
  vni: number
  subnet_count?: number
}

export interface Subnet {
  metadata: Metadata
  vpc_id: Id
  vpc_name?: string
  cidr: string
  netns: string
}

// ── Load balancer (ocf-loadbalancer) ────────────────────────────────────────

export type LbKind = 'Tcp' | 'Application'
export type RoutingPolicy = 'RoundRobin' | 'LeastLoad' | 'Latency' | 'Geo'

export interface Listener {
  port: number
  tls: boolean
}

export interface LoadBalancer {
  metadata: Metadata
  kind: LbKind
  listeners: Listener[]
  target_selector?: Record<string, string>
  policy: RoutingPolicy
  placement?: string | null
  anycast: boolean
  hostnames: string[]
  backend_count?: number
  health?: Health
}

// ── Disk (ocf-disk) ─────────────────────────────────────────────────────────

export type DiskHealth = 'Ok' | 'Warning' | 'Failing' | 'Unknown'
export type LedState = 'Normal' | 'Locate' | 'Fault' | 'Rebuild'

export interface PhysicalDisk {
  metadata: Metadata
  machine_id: Id
  machine_name?: string
  dev_path: string
  serial: string
  wwn?: string
  model: string
  vendor: string
  size_bytes: number
  health: DiskHealth
  first_seen?: string
  rma_date?: string | null
  enclosure?: string | null
  slot?: number | null
  led?: LedState
}

// ── Monitoring (ocf-monitoring) ─────────────────────────────────────────────

export interface ResourceUsage {
  cpu_pct: number
  memory_used: number
  memory_total: number
  disk_used: number
  disk_total: number
  net_rx_bps: number
  net_tx_bps: number
  read_iops: number
  write_iops: number
}

// ── Authz (ocf-authz) ───────────────────────────────────────────────────────

export interface User {
  metadata: Metadata
  username: string
  groups: string[]
  email?: string
}

export interface Role {
  metadata: Metadata
  permissions: string[]
}

export interface Group {
  metadata: Metadata
  members: string[]
}

// ── Health endpoint ─────────────────────────────────────────────────────────

export interface HealthReport {
  status: 'ok' | 'degraded' | 'down'
  version?: string
}
