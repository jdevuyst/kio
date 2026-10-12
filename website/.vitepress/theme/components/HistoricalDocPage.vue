<script setup>
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { withBase } from 'vitepress'
import SourceFooter from './SourceFooter.vue'

const props = defineProps({
  article: { type: Object, required: true },
  html: { type: String, default: '' }
})
const hash = ref('')
const updateHash = () => { hash.value = window.location.hash }
onMounted(() => {
  updateHash()
  window.addEventListener('hashchange', updateHash)
})
onUnmounted(() => window.removeEventListener('hashchange', updateHash))
const currentUrl = computed(() => props.article.currentRoute ? withBase(props.article.currentRoute) + hash.value : null)
const provenance = computed(() => props.article.releases.length
  ? `for Kio ${props.article.releases.map(release => release.version).join(', ')}`
  : `at commit ${props.article.revision.slice(0, 8)}`)
const contentState = computed(() => !props.article.currentAvailable
  ? 'This page has no counterpart in the current documentation.'
  : props.article.differs ? 'This page has changed since then.' : 'This page matches the current documentation.')
</script>

<template>
  <div class="historical-doc">
    <div class="history-banner">
      <p class="history-content-notice">You’re reading documentation {{ provenance }}. {{ contentState }}</p>
      <p v-if="currentUrl" class="history-current-action"><a :href="currentUrl">View current page</a></p>
      <p v-else class="history-current-action"><a :href="withBase('/docs/')">Browse current docs</a></p>
    </div>
    <div class="kiodoc-import" v-html="html" />
    <SourceFooter :source="{ revision: article.revision, url: article.sourceUrl }" />
  </div>
</template>

<style scoped>
.history-banner {
  max-width: var(--kio-doc-prose-width);
  margin-bottom: 28px;
  padding: 14px 18px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  background: var(--vp-c-brand-soft);
}

.history-current-action {
  margin: 10px 0 0;
  font-size: 14px;
}

.history-current-action a:focus-visible {
  outline: 2px solid var(--vp-c-brand-1);
  outline-offset: 2px;
}

.history-content-notice {
  margin: 0;
  font-size: 14px;
}
</style>
