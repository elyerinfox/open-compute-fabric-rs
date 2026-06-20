<script setup lang="ts" generic="T extends Record<string, any>">
// Generic, styled table. Columns describe how to read each row; cells can be
// overridden with a per-column scoped slot named `cell-<key>`.
interface Column {
  key: string
  label: string
  /** Tailwind text alignment, default left. */
  align?: 'left' | 'right' | 'center'
  /** Render value in a monospace font (ids, cidrs, etc.). */
  mono?: boolean
  class?: string
}

const props = defineProps<{
  columns: Column[]
  rows: T[]
  rowKey?: keyof T | ((row: T) => string)
  empty?: string
}>()

function keyFor(row: T, i: number): string {
  if (typeof props.rowKey === 'function') return props.rowKey(row)
  if (props.rowKey) return String(row[props.rowKey])
  return String(i)
}

function alignClass(c: Column): string {
  return c.align === 'right' ? 'text-right' : c.align === 'center' ? 'text-center' : 'text-left'
}
</script>

<template>
  <div class="ocf-card overflow-hidden">
    <div class="overflow-x-auto">
      <table class="min-w-full divide-y divide-surface-700 text-sm">
        <thead class="bg-surface-850/60">
          <tr>
            <th
              v-for="col in columns"
              :key="col.key"
              scope="col"
              class="whitespace-nowrap px-4 py-3 text-xs font-semibold uppercase tracking-wide text-slate-400"
              :class="[alignClass(col), col.class]"
            >
              {{ col.label }}
            </th>
          </tr>
        </thead>
        <tbody class="divide-y divide-surface-800">
          <tr
            v-for="(row, i) in rows"
            :key="keyFor(row, i)"
            class="transition-colors hover:bg-surface-800/50"
          >
            <td
              v-for="col in columns"
              :key="col.key"
              class="whitespace-nowrap px-4 py-3 text-slate-300"
              :class="[alignClass(col), col.mono ? 'font-mono text-xs text-slate-400' : '', col.class]"
            >
              <slot :name="`cell-${col.key}`" :row="row" :value="row[col.key]">
                {{ row[col.key] ?? '—' }}
              </slot>
            </td>
          </tr>
          <tr v-if="rows.length === 0">
            <td
              :colspan="columns.length"
              class="px-4 py-10 text-center text-sm text-slate-500"
            >
              {{ empty ?? 'No records.' }}
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
