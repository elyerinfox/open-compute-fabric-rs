<script setup lang="ts">
import type { HealthFinding, Severity } from '~/composables/types'

const api = useApi()
const { date } = useFormat()

// Fetch the fleet-health findings for this node. useAsyncData caches per-key and
// runs on the server for the first paint; `refresh()` re-runs it after a fix.
const { data, refresh } = await useAsyncData('health', () => api.getHealthFindings())

const findings = computed(() => data.value?.data ?? [])
const source = computed<'live' | 'mock'>(() => data.value?.source ?? 'mock')

// Sort most-severe first so critical problems lead. Ties keep API order.
const SEVERITY_RANK: Record<Severity, number> = { critical: 0, warning: 1, info: 2 }
const sortedFindings = computed(() =>
  [...findings.value].sort(
    (a, b) => (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9),
  ),
)

const counts = computed(() => ({
  critical: findings.value.filter((f) => f.severity === 'critical').length,
  warning: findings.value.filter((f) => f.severity === 'warning').length,
  info: findings.value.filter((f) => f.severity === 'info').length,
}))

// Per-fix UI state: whether a fix is being applied, and the last result line.
const applying = reactive<Record<string, boolean>>({})
const results = reactive<Record<string, { applied: boolean; outcome: string }>>({})

function fixKey(finding: HealthFinding, fixId: string): string {
  return `${finding.id}::${fixId}`
}

async function applyFix(finding: HealthFinding, fixId: string) {
  const key = fixKey(finding, fixId)
  applying[key] = true
  delete results[key]
  try {
    const r = await api.applyHealthFix(finding.check, fixId)
    results[key] = r.data
  } finally {
    applying[key] = false
  }
  // Re-run the findings query so a successful fix drops the finding.
  await refresh()
}
</script>

<template>
  <div>
    <PageHeader
      title="Fleet Health"
      subtitle="Node-level warnings detected across the fabric — apply a fix inline to remediate."
      :source="source"
    />

    <!-- Summary -->
    <div class="mb-6 grid grid-cols-2 gap-4 sm:grid-cols-4">
      <StatCard label="Findings" :value="findings.length" accent="brand" />
      <StatCard label="Critical" :value="counts.critical" :accent="counts.critical ? 'rose' : 'emerald'" />
      <StatCard label="Warning" :value="counts.warning" :accent="counts.warning ? 'amber' : 'emerald'" />
      <StatCard label="Info" :value="counts.info" accent="sky" />
    </div>

    <!-- Empty state: all checks passing -->
    <div
      v-if="findings.length === 0"
      class="ocf-card flex flex-col items-center justify-center px-6 py-16 text-center"
    >
      <div class="flex h-14 w-14 items-center justify-center rounded-full bg-emerald-500/15 text-emerald-400">
        <svg class="h-7 w-7" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
        </svg>
      </div>
      <h2 class="mt-4 text-lg font-semibold text-slate-100">All checks passing on this node</h2>
      <p class="mt-1 text-sm text-slate-400">No fleet-health findings were detected.</p>
    </div>

    <!-- Findings, most severe first -->
    <div v-else class="space-y-4">
      <article
        v-for="finding in sortedFindings"
        :key="finding.id"
        class="ocf-card p-5"
      >
        <div class="flex flex-wrap items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="flex flex-wrap items-center gap-2">
              <HealthBadge :status="finding.severity" />
              <h3 class="text-base font-semibold text-slate-100">{{ finding.title }}</h3>
            </div>
            <p class="mt-1 font-mono text-xs text-slate-500">
              {{ finding.check }} · {{ finding.machine_id }}
            </p>
          </div>
          <span class="whitespace-nowrap text-xs text-slate-600">
            detected {{ date(finding.detected_at) }}
          </span>
        </div>

        <p class="mt-3 text-sm text-slate-400">{{ finding.detail }}</p>

        <!-- Fixes -->
        <div v-if="finding.fixes.length" class="mt-4 space-y-3 border-t border-surface-800 pt-4">
          <div
            v-for="fix in finding.fixes"
            :key="fix.id"
            class="flex flex-wrap items-start justify-between gap-3"
          >
            <div class="min-w-0">
              <p class="text-sm font-medium text-slate-200">{{ fix.label }}</p>
              <p class="mt-0.5 text-xs text-slate-500">{{ fix.description }}</p>
              <p v-if="fix.requires_root" class="mt-1 inline-flex items-center gap-1 text-[11px] text-amber-400/80">
                <svg class="h-3 w-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                  <path stroke-linecap="round" stroke-linejoin="round" d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zM16 11V7a4 4 0 00-8 0v4" />
                </svg>
                requires root
              </p>

              <!-- Inline result -->
              <p
                v-if="results[fixKey(finding, fix.id)]"
                class="mt-2 text-xs"
                :class="results[fixKey(finding, fix.id)].applied ? 'text-emerald-400' : 'text-rose-400'"
              >
                {{ results[fixKey(finding, fix.id)].outcome }}
              </p>
            </div>

            <button
              type="button"
              class="inline-flex flex-none items-center gap-2 rounded-md border border-surface-600 px-3 py-1.5 text-xs font-medium text-slate-200 transition-colors hover:border-brand-500 hover:text-brand-400 disabled:cursor-not-allowed disabled:opacity-60"
              :disabled="applying[fixKey(finding, fix.id)]"
              @click="applyFix(finding, fix.id)"
            >
              <svg
                v-if="applying[fixKey(finding, fix.id)]"
                class="h-3.5 w-3.5 animate-spin"
                viewBox="0 0 24 24"
                fill="none"
              >
                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.4 0 0 5.4 0 12h4z" />
              </svg>
              {{ applying[fixKey(finding, fix.id)] ? 'Applying…' : fix.label }}
            </button>
          </div>
        </div>
      </article>
    </div>
  </div>
</template>
