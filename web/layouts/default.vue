<script setup lang="ts">
// Dashboard shell: fixed sidebar nav + scrollable content area.
// Collapsible on mobile via a hamburger toggle.
const route = useRoute()
const mobileOpen = ref(false)

interface NavItem {
  to: string
  label: string
  // Heroicons-style outline path data.
  icon: string
}

const nav: NavItem[] = [
  { to: '/', label: 'Dashboard', icon: 'M3 12l9-9 9 9M5 10v10a1 1 0 001 1h4v-6h4v6h4a1 1 0 001-1V10' },
  { to: '/health', label: 'Health', icon: 'M3 12h4l2 5 4-12 2 7h6' },
  { to: '/topology', label: 'Topology', icon: 'M4 5h6v6H4V5zm10 8h6v6h-6v-6zM7 11v4a2 2 0 002 2h4' },
  { to: '/workloads', label: 'Workloads', icon: 'M4 6h16M4 12h16M4 18h16' },
  { to: '/networking', label: 'Networking', icon: 'M12 3v18m0-18a4 4 0 014 4c0 2-2 3-4 3s-4-1-4-3a4 4 0 014-4zm-8 9h16' },
  { to: '/loadbalancers', label: 'Load Balancers', icon: 'M12 3v6m0 0L8 13m4-4l4 4M5 13h14a2 2 0 012 2v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4a2 2 0 012-2z' },
  { to: '/storage', label: 'Storage', icon: 'M4 7c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zm0 0v10c0 1.7 3.6 3 8 3s8-1.3 8-3V7' },
  { to: '/access', label: 'Access', icon: 'M17 20h5v-2a4 4 0 00-3-3.87M9 20H4v-2a4 4 0 013-3.87m6-1.13a4 4 0 10-4-4 4 4 0 004 4z' },
]

function isActive(to: string): boolean {
  if (to === '/') return route.path === '/'
  return route.path.startsWith(to)
}
</script>

<template>
  <div class="min-h-screen bg-surface-950 text-slate-200">
    <!-- Mobile top bar -->
    <header
      class="sticky top-0 z-30 flex items-center gap-3 border-b border-surface-800 bg-surface-900/90 px-4 py-3 backdrop-blur lg:hidden"
    >
      <button
        type="button"
        class="rounded-lg p-1.5 text-slate-400 hover:bg-surface-800 hover:text-slate-100"
        aria-label="Toggle navigation"
        @click="mobileOpen = !mobileOpen"
      >
        <svg class="h-6 w-6" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M4 6h16M4 12h16M4 18h16" />
        </svg>
      </button>
      <span class="font-semibold tracking-tight text-slate-100">Open Compute Fabric</span>
    </header>

    <!-- Mobile backdrop -->
    <div
      v-if="mobileOpen"
      class="fixed inset-0 z-30 bg-black/50 lg:hidden"
      @click="mobileOpen = false"
    />

    <div class="lg:flex">
      <!-- Sidebar -->
      <aside
        class="fixed inset-y-0 left-0 z-40 w-64 transform border-r border-surface-800 bg-surface-900 transition-transform duration-200 lg:static lg:translate-x-0"
        :class="mobileOpen ? 'translate-x-0' : '-translate-x-full'"
      >
        <div class="flex h-full flex-col">
          <!-- Brand -->
          <div class="flex items-center gap-3 px-5 py-5">
            <div
              class="flex h-9 w-9 items-center justify-center rounded-lg bg-brand-600 text-white shadow-lg shadow-brand-600/30"
            >
              <svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path stroke-linecap="round" stroke-linejoin="round" d="M4 5h6v6H4V5zm10 0h6v6h-6V5zM4 15h6v4H4v-4zm10 0h6v4h-6v-4z" />
              </svg>
            </div>
            <div class="leading-tight">
              <p class="text-sm font-semibold text-slate-100">Open Compute</p>
              <p class="text-xs text-slate-500">Fabric Control Plane</p>
            </div>
          </div>

          <!-- Nav -->
          <nav class="flex-1 space-y-1 px-3 py-2">
            <NuxtLink
              v-for="item in nav"
              :key="item.to"
              :to="item.to"
              class="ocf-nav-link"
              :class="isActive(item.to) ? 'ocf-nav-link-active' : ''"
              @click="mobileOpen = false"
            >
              <svg class="h-5 w-5 flex-none" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
                <path stroke-linecap="round" stroke-linejoin="round" :d="item.icon" />
              </svg>
              {{ item.label }}
            </NuxtLink>
          </nav>

          <!-- Footer / status -->
          <div class="border-t border-surface-800 px-5 py-4">
            <div class="flex items-center gap-2 text-xs text-slate-500">
              <span class="h-2 w-2 animate-pulse rounded-full bg-emerald-400" />
              <span>contract-first · no quorum</span>
            </div>
            <p class="mt-1 text-[11px] text-slate-600">v0.1.0-dev</p>
          </div>
        </div>
      </aside>

      <!-- Content -->
      <main class="min-w-0 flex-1">
        <div class="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
          <slot />
        </div>
      </main>
    </div>
  </div>
</template>
