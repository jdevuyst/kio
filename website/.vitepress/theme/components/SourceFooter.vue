<script setup>
defineProps({
  source: { type: Object, default: null }
})
</script>

<template>
  <footer v-if="source" class="source-footer">
    <a :href="source.url">View source on GitHub</a>
  </footer>
</template>

<style scoped>
.source-footer {
  max-width: var(--kio-doc-prose-width);
  margin-top: 32px;
  padding-top: 16px;
  border-top: 1px solid var(--vp-c-divider);
  font-size: 13px;
  line-height: 1.7;
  color: var(--vp-c-text-2);
}

:global(.kiodoc-import:has(.blog-post-footer) + .source-footer) {
  margin-top: 16px;
  padding-top: 0;
  border-top: 0;
}
</style>
