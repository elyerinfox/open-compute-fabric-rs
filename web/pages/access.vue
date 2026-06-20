<script setup lang="ts">
const api = useApi()

const { data } = await useAsyncData('access', async () => {
  const [users, roles] = await Promise.all([api.listUsers(), api.listRoles()])
  return { users, roles }
})

const users = computed(() => data.value?.users.data ?? [])
const roles = computed(() => data.value?.roles.data ?? [])
const source = computed<'live' | 'mock'>(() =>
  data.value && data.value.users.source === 'live' && data.value.roles.source === 'live'
    ? 'live'
    : 'mock',
)

const tab = ref<'users' | 'roles'>('users')
</script>

<template>
  <div>
    <PageHeader
      title="Access"
      subtitle="Scoped RBAC — users, groups, and roles (Proxmox-style)."
      :source="source"
    />

    <!-- Tabs -->
    <div class="mb-4 inline-flex rounded-lg border border-surface-700 bg-surface-900 p-0.5">
      <button
        type="button"
        class="rounded-md px-4 py-1.5 text-sm font-medium transition-colors"
        :class="tab === 'users' ? 'bg-brand-600 text-white' : 'text-slate-400 hover:text-slate-200'"
        @click="tab = 'users'"
      >
        Users
      </button>
      <button
        type="button"
        class="rounded-md px-4 py-1.5 text-sm font-medium transition-colors"
        :class="tab === 'roles' ? 'bg-brand-600 text-white' : 'text-slate-400 hover:text-slate-200'"
        @click="tab = 'roles'"
      >
        Roles
      </button>
    </div>

    <!-- Users -->
    <ResourceTable
      v-if="tab === 'users'"
      :columns="[
        { key: 'username', label: 'Username', mono: true },
        { key: 'name', label: 'Display name' },
        { key: 'email', label: 'Email' },
        { key: 'groups', label: 'Groups' },
      ]"
      :rows="users"
      :row-key="(u) => u.metadata.id"
      empty="No users."
    >
      <template #cell-username="{ row }">
        <span class="text-slate-300">{{ row.username }}</span>
      </template>
      <template #cell-name="{ row }">
        <span class="font-medium text-slate-200">{{ row.metadata.name }}</span>
      </template>
      <template #cell-email="{ row }">
        <span class="text-xs text-slate-400">{{ row.email ?? '—' }}</span>
      </template>
      <template #cell-groups="{ row }">
        <div class="flex flex-wrap gap-1">
          <span
            v-for="g in row.groups"
            :key="g"
            class="rounded bg-surface-800 px-1.5 py-0.5 font-mono text-xs text-slate-300"
          >
            {{ g }}
          </span>
        </div>
      </template>
    </ResourceTable>

    <!-- Roles -->
    <div v-else class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
      <div v-for="role in roles" :key="role.metadata.id" class="ocf-card p-5">
        <div class="flex items-center justify-between">
          <h3 class="font-semibold text-slate-100">{{ role.metadata.name }}</h3>
          <span
            v-if="role.permissions.includes('*')"
            class="rounded bg-rose-500/15 px-2 py-0.5 text-[10px] font-semibold uppercase text-rose-400"
          >
            superuser
          </span>
        </div>
        <p class="mt-3 text-xs font-medium text-slate-500">
          {{ role.permissions.length }} {{ role.permissions.length === 1 ? 'permission' : 'permissions' }}
        </p>
        <div class="mt-2 flex flex-wrap gap-1.5">
          <span
            v-for="perm in role.permissions"
            :key="perm"
            class="rounded-md bg-surface-800 px-2 py-0.5 font-mono text-xs"
            :class="perm === '*' ? 'text-rose-400' : 'text-slate-300'"
          >
            {{ perm }}
          </span>
        </div>
      </div>
    </div>

    <p class="mt-4 text-xs text-slate-500">
      A role binding grants a role to a user or group at a scope; the grant covers everything beneath
      that scope in the fleet → region → datacenter → rack → machine hierarchy.
    </p>
  </div>
</template>
