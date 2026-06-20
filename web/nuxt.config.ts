// https://nuxt.com/docs/api/configuration/nuxt-config
export default defineNuxtConfig({
  compatibilityDate: '2024-11-01',
  devtools: { enabled: true },

  modules: ['@nuxtjs/tailwindcss'],

  // Dark theme is applied globally via the `dark` class on <html>.
  app: {
    head: {
      htmlAttrs: { lang: 'en', class: 'dark' },
      title: 'Open Compute Fabric',
      meta: [
        { charset: 'utf-8' },
        { name: 'viewport', content: 'width=device-width, initial-scale=1' },
        {
          name: 'description',
          content: 'Open Compute Fabric — contract-first hypervisor & fleet control plane',
        },
      ],
    },
  },

  // The API base URL is exposed to the client and can be overridden at runtime
  // with NUXT_PUBLIC_API_BASE (e.g. when running behind a different host).
  runtimeConfig: {
    public: {
      apiBase: 'http://localhost:8080/api/v1',
    },
  },

  // Dev-time proxy: forward `/api/**` to the ocf-api backend so the browser
  // can call same-origin and avoid CORS while developing.
  nitro: {
    devProxy: {
      '/api': {
        target: 'http://localhost:8080/api',
        changeOrigin: true,
      },
    },
  },

  typescript: {
    strict: true,
  },
})
