<script setup lang="ts">
// Recursive topology tree row. Renders one TopologyNode and, when expanded,
// its children (each another <TreeNode>). Clicking a node emits `select`;
// the topology page uses that to populate the detail panel.
import type { TopologyNode } from '~/composables/types'

const props = defineProps<{
  node: TopologyNode
  selectedId?: string | null
  depth?: number
}>()

const emit = defineEmits<{
  (e: 'select', node: TopologyNode): void
}>()

const depth = computed(() => props.depth ?? 0)
const hasChildren = computed(() => props.node.children && props.node.children.length > 0)

// Top three levels start expanded so the fleet is visible at a glance.
const open = ref((props.depth ?? 0) < 3)

const LEVEL_ICON: Record<string, string> = {
  Fleet: 'M3 3h18v4H3V3zm0 7h18v4H3v-4zm0 7h18v4H3v-4z',
  Region: 'M12 2a10 10 0 100 20 10 10 0 000-20zm0 0v20m-10-10h20',
  Datacenter: 'M4 4h16v6H4V4zm0 10h16v6H4v-6z',
  Rack: 'M5 3h14v18H5V3zm0 5h14M5 13h14',
  Machine: 'M4 5h16v10H4V5zm-1 14h18',
}

const LEVEL_LABEL: Record<string, string> = {
  Fleet: 'Fleet',
  Region: 'Region',
  Datacenter: 'DC',
  Rack: 'Rack',
  Machine: 'Machine',
}

function onActivate() {
  if (hasChildren.value) open.value = !open.value
  emit('select', props.node)
}

const isSelected = computed(() => props.selectedId === props.node.id)
</script>

<template>
  <div>
    <button
      type="button"
      class="group flex w-full items-center gap-2 rounded-lg py-1.5 pr-2 text-left transition-colors hover:bg-surface-800"
      :class="isSelected ? 'bg-brand-600/15 ring-1 ring-inset ring-brand-600/30' : ''"
      :style="{ paddingLeft: `${depth * 18 + 8}px` }"
      @click="onActivate"
    >
      <!-- Expand chevron (or spacer for leaves) -->
      <span class="flex h-4 w-4 flex-none items-center justify-center text-slate-500">
        <svg
          v-if="hasChildren"
          class="h-3.5 w-3.5 transition-transform"
          :class="open ? 'rotate-90' : ''"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.5"
        >
          <path stroke-linecap="round" stroke-linejoin="round" d="M9 5l7 7-7 7" />
        </svg>
      </span>

      <!-- Level icon -->
      <span class="flex h-5 w-5 flex-none items-center justify-center text-slate-500 group-hover:text-slate-300">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" class="h-4 w-4">
          <path stroke-linecap="round" stroke-linejoin="round" :d="LEVEL_ICON[node.level]" />
        </svg>
      </span>

      <span class="flex-none text-[10px] font-semibold uppercase tracking-wide text-slate-600">
        {{ LEVEL_LABEL[node.level] }}
      </span>

      <span
        class="truncate text-sm font-medium"
        :class="isSelected ? 'text-brand-300' : 'text-slate-200'"
      >
        {{ node.name }}
      </span>

      <span class="ml-auto flex-none">
        <HealthBadge :status="node.health" dot />
      </span>
    </button>

    <div v-if="open && hasChildren" class="mt-0.5 space-y-0.5">
      <TreeNode
        v-for="child in node.children"
        :key="child.id"
        :node="child"
        :selected-id="selectedId"
        :depth="depth + 1"
        @select="(n) => emit('select', n)"
      />
    </div>
  </div>
</template>
