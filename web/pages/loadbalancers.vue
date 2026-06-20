<script setup lang="ts">
const api = useApi()

const { data } = await useAsyncData('loadbalancers', () => api.listLoadBalancers())
const lbs = computed(() => data.value?.data ?? [])
const source = computed<'live' | 'mock'>(() => data.value?.source ?? 'mock')

const POLICY_LABEL: Record<string, string> = {
  RoundRobin: 'Round-robin',
  LeastLoad: 'Least load',
  Latency: 'Latency-based',
  Geo: 'Geographic',
}
</script>

<template>
  <div>
    <PageHeader
      title="Load Balancers"
      subtitle="TCP and application load balancers routing across the fabric."
      :source="source"
    />

    <div class="grid grid-cols-1 gap-4 lg:grid-cols-2">
      <div v-for="lb in lbs" :key="lb.metadata.id" class="ocf-card p-5">
        <div class="flex items-start justify-between gap-3">
          <div>
            <div class="flex items-center gap-2">
              <h3 class="font-semibold text-slate-100">{{ lb.metadata.name }}</h3>
              <span
                class="rounded bg-surface-800 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-slate-400"
              >
                {{ lb.kind === 'Application' ? 'L7 ALB' : 'L4 TCP' }}
              </span>
              <span
                v-if="lb.anycast"
                class="rounded bg-sky-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-sky-400"
                title="Reachable on every public IP in scope"
              >
                Anycast
              </span>
            </div>
            <p class="mt-1 text-xs text-slate-500">
              {{ POLICY_LABEL[lb.policy] ?? lb.policy }} routing
            </p>
          </div>
          <HealthBadge v-if="lb.health" :status="lb.health" />
        </div>

        <!-- Listeners -->
        <div class="mt-4">
          <p class="text-xs font-medium text-slate-500">Listeners</p>
          <div class="mt-2 flex flex-wrap gap-1.5">
            <span
              v-for="l in lb.listeners"
              :key="l.port"
              class="inline-flex items-center gap-1 rounded-md bg-surface-800 px-2 py-1 font-mono text-xs text-slate-300"
            >
              :{{ l.port }}
              <span v-if="l.tls" class="text-emerald-400" title="TLS terminated">🔒</span>
            </span>
          </div>
        </div>

        <!-- Hostnames -->
        <div v-if="lb.hostnames.length" class="mt-4">
          <p class="text-xs font-medium text-slate-500">Hostnames</p>
          <div class="mt-2 space-y-1">
            <p v-for="h in lb.hostnames" :key="h" class="font-mono text-xs text-slate-400">{{ h }}</p>
          </div>
        </div>

        <!-- Footer meta -->
        <div class="mt-4 flex items-center justify-between border-t border-surface-800 pt-3 text-xs">
          <span class="text-slate-500">
            Placement:
            <span class="font-mono text-slate-400">{{ lb.placement ?? 'fleet-wide' }}</span>
          </span>
          <span class="text-slate-400">{{ lb.backend_count ?? 0 }} backends</span>
        </div>
      </div>
    </div>

    <p class="mt-4 text-xs text-slate-500">
      Placement scope restricts where target workloads may live and migrate. Application LBs can
      terminate TLS and auto-renew certificates (Let's Encrypt) with Dynamic DNS upserts.
    </p>
  </div>
</template>
