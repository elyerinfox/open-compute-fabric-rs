// Typed fetch client for the ocf-api REST surface.
//
// Every method targets a documented endpoint under the configured base URL
// (default http://localhost:8080/api/v1, overridable via runtimeConfig /
// NUXT_PUBLIC_API_BASE). When a request fails for any reason — the backend
// isn't running, returns a non-2xx status, or yields unparseable JSON — the
// client transparently falls back to the bundled MOCK fixtures so the UI
// always renders. Each method returns `{ data, source }` where `source` tells
// the caller whether the data is 'live' or 'mock'.

import type {
  HealthReport,
  HealthFinding,
  TopologyNode,
  MachineDetail,
  Workload,
  Vpc,
  Subnet,
  LoadBalancer,
  PhysicalDisk,
  ResourceUsage,
  User,
  Role,
} from './types'

import {
  mockHealth,
  mockHealthFindings,
  mockTopology,
  mockWorkloads,
  mockVpcs,
  mockSubnets,
  mockLoadBalancers,
  mockDisks,
  mockHostMetrics,
  mockUsers,
  mockRoles,
} from './mockData'

export type DataSource = 'live' | 'mock'

export interface ApiResult<T> {
  data: T
  source: DataSource
}

/** Capitalize the lowercase enum strings the API emits (e.g. `healthy` → `Healthy`). */
function titleCase(s?: string): string {
  if (!s) return 'Unknown'
  return s.charAt(0).toUpperCase() + s.slice(1)
}

/** Convert serde snake_case enum strings to the PascalCase the UI types use
 *  (`virtual_machine` → `VirtualMachine`, `round_robin` → `RoundRobin`). */
function snakeToPascal(s?: string): string {
  if (!s) return 'Unknown'
  return s
    .split('_')
    .map((p) => p.charAt(0).toUpperCase() + p.slice(1))
    .join('')
}

function adaptWorkload(w: any): Workload {
  return {
    ...w,
    kind: snakeToPascal(w?.kind) as Workload['kind'],
    state: snakeToPascal(w?.state) as Workload['state'],
    // The API carries the placement node id in `node`; surface it as a name too.
    node_name: w?.node_name ?? (typeof w?.node === 'string' ? w.node : null),
  }
}

function adaptDisk(d: any): PhysicalDisk {
  return {
    ...d,
    health: snakeToPascal(d?.health) as PhysicalDisk['health'],
    led: d?.led ? (snakeToPascal(d.led) as PhysicalDisk['led']) : undefined,
  }
}

function adaptLoadBalancer(lb: any): LoadBalancer {
  return {
    ...lb,
    kind: snakeToPascal(lb?.kind) as LoadBalancer['kind'],
    policy: snakeToPascal(lb?.policy) as LoadBalancer['policy'],
  }
}

/**
 * The live API returns the topology as `ocf-topology::TopologyTree`
 * (`{ regions: [{ region, datacenters: [{ datacenter, racks: [{ rack, machines }] }] }] }`).
 * Adapt it into the recursive `TopologyNode` the UI renders, with a synthetic
 * "Fleet" root. Defensive throughout so a partial payload can't crash SSR.
 */
function adaptTopology(raw: any): TopologyNode {
  const regions = Array.isArray(raw?.regions) ? raw.regions : []
  return {
    id: 'fleet',
    name: 'Fleet',
    level: 'Fleet',
    health: 'Healthy',
    children: regions.map((rn: any) => ({
      id: rn?.region?.metadata?.id ?? 'region',
      name: rn?.region?.metadata?.name ?? 'region',
      level: 'Region' as const,
      health: 'Healthy' as const,
      children: (rn?.datacenters ?? []).map((dn: any) => ({
        id: dn?.datacenter?.metadata?.id ?? 'datacenter',
        name: dn?.datacenter?.metadata?.name ?? 'datacenter',
        level: 'Datacenter' as const,
        health: 'Healthy' as const,
        children: (dn?.racks ?? []).map((rk: any) => ({
          id: rk?.rack?.metadata?.id ?? 'rack',
          name: rk?.rack?.metadata?.name ?? 'rack',
          level: 'Rack' as const,
          health: 'Healthy' as const,
          children: (rk?.machines ?? []).map((m: any) => {
            const cap = m?.capacity ?? {}
            return {
              id: m?.metadata?.id ?? 'machine',
              name: m?.metadata?.name ?? 'machine',
              level: 'Machine' as const,
              health: titleCase(m?.health) as TopologyNode['health'],
              machine: {
                id: m?.metadata?.id ?? '',
                hostname: m?.metadata?.name ?? '',
                health: titleCase(m?.health) as TopologyNode['health'],
                state: titleCase(m?.state) as MachineDetail['state'],
                cpu_cores: Math.round((cap.cpu_millis ?? 0) / 1000),
                memory_bytes: cap.memory_bytes ?? 0,
                disk_bytes: cap.disk_bytes ?? 0,
                ip_address: m?.fabric_address ?? undefined,
                rack: rk?.rack?.metadata?.name ?? undefined,
                workload_count: 0,
                labels: m?.metadata?.labels ?? {},
              },
              children: [],
            }
          }),
        })),
      })),
    })),
  }
}

export function useApi() {
  const config = useRuntimeConfig()
  const baseURL = (config.public.apiBase as string) || 'http://localhost:8080/api/v1'

  /**
   * Fetch `path` and parse JSON, falling back to `fallback` on any error.
   * Uses $fetch (ofetch) which Nuxt provides on both server and client.
   */
  async function getOrMock<T>(path: string, fallback: T): Promise<ApiResult<T>> {
    try {
      const data = await $fetch<T>(path, {
        baseURL,
        // Keep the UI snappy; a dead backend should fall back fast.
        timeout: 4000,
        retry: 0,
      })
      return { data, source: 'live' }
    } catch (err) {
      if (import.meta.dev) {
        // eslint-disable-next-line no-console
        console.warn(`[useApi] ${path} unavailable, using mock data:`, (err as Error)?.message)
      }
      return { data: fallback, source: 'mock' }
    }
  }

  return {
    baseURL,

    /** GET /api/v1/health */
    getHealth(): Promise<ApiResult<HealthReport>> {
      return getOrMock('/health', mockHealth)
    },

    /** GET /api/v1/health/findings — fleet-health checks for this node. */
    getHealthFindings(): Promise<ApiResult<HealthFinding[]>> {
      return getOrMock('/health/findings', mockHealthFindings)
    },

    /**
     * POST /api/v1/health/fix with body `{ check, fix }`.
     * Applies a finding's remediation. When the backend is unreachable, reports
     * a mock failure so the UI can surface "(backend unavailable)" rather than
     * implying the fix ran.
     */
    async applyHealthFix(
      check: string,
      fix: string,
    ): Promise<ApiResult<{ applied: boolean; outcome: string }>> {
      try {
        const data = await $fetch<{ applied: boolean; outcome: string }>('/health/fix', {
          baseURL,
          method: 'POST',
          body: { check, fix },
          timeout: 4000,
          retry: 0,
        })
        return { data, source: 'live' }
      } catch {
        return { data: { applied: false, outcome: '(backend unavailable)' }, source: 'mock' }
      }
    },

    /** GET /api/v1/topology/tree — adapts the API's `{ regions: [...] }` shape. */
    async getTopologyTree(): Promise<ApiResult<TopologyNode>> {
      try {
        const raw = await $fetch<any>('/topology/tree', { baseURL, timeout: 4000, retry: 0 })
        // Live API: { regions: [...] } → adapt. Mock: already a TopologyNode.
        const data =
          raw && Array.isArray(raw.regions) ? adaptTopology(raw) : (raw as TopologyNode)
        return { data, source: 'live' }
      } catch (err) {
        if (import.meta.dev) {
          // eslint-disable-next-line no-console
          console.warn('[useApi] /topology/tree unavailable, using mock data:', (err as Error)?.message)
        }
        return { data: mockTopology, source: 'mock' }
      }
    },

    /** GET /api/v1/workloads */
    async listWorkloads(): Promise<ApiResult<Workload[]>> {
      const r = await getOrMock<Workload[]>('/workloads', mockWorkloads)
      return r.source === 'live' ? { data: r.data.map(adaptWorkload), source: r.source } : r
    },

    /** GET /api/v1/networks/vpcs */
    listVpcs(): Promise<ApiResult<Vpc[]>> {
      return getOrMock('/networks/vpcs', mockVpcs)
    },

    /** GET /api/v1/networks/subnets */
    listSubnets(): Promise<ApiResult<Subnet[]>> {
      return getOrMock('/networks/subnets', mockSubnets)
    },

    /** GET /api/v1/loadbalancers */
    async listLoadBalancers(): Promise<ApiResult<LoadBalancer[]>> {
      const r = await getOrMock<LoadBalancer[]>('/loadbalancers', mockLoadBalancers)
      return r.source === 'live' ? { data: r.data.map(adaptLoadBalancer), source: r.source } : r
    },

    /** GET /api/v1/disks */
    async listDisks(): Promise<ApiResult<PhysicalDisk[]>> {
      const r = await getOrMock<PhysicalDisk[]>('/disks', mockDisks)
      return r.source === 'live' ? { data: r.data.map(adaptDisk), source: r.source } : r
    },

    /** GET /api/v1/metrics/host */
    getHostMetrics(): Promise<ApiResult<ResourceUsage>> {
      return getOrMock('/metrics/host', mockHostMetrics)
    },

    /** GET /api/v1/access/users */
    listUsers(): Promise<ApiResult<User[]>> {
      return getOrMock('/access/users', mockUsers)
    },

    /** GET /api/v1/access/roles */
    listRoles(): Promise<ApiResult<Role[]>> {
      return getOrMock('/access/roles', mockRoles)
    },

    /**
     * POST /api/v1/workloads/{id}/migrate.
     * Returns mock success when the backend is unavailable so the UI flow works.
     */
    async migrateWorkload(id: string, target?: string): Promise<ApiResult<{ accepted: boolean }>> {
      try {
        const data = await $fetch<{ accepted: boolean }>(`/workloads/${id}/migrate`, {
          baseURL,
          method: 'POST',
          body: { target },
          timeout: 4000,
          retry: 0,
        })
        return { data, source: 'live' }
      } catch {
        return { data: { accepted: true }, source: 'mock' }
      }
    },
  }
}
