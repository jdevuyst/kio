<script setup>
import { computed } from 'vue'
import { kiodocHtml } from '../../generated/kiodoc-html.mjs'

const props = defineProps({
  page: {
    type: String,
    required: true
  }
})

const html = computed(() => kiodocHtml[props.page] ?? '<p>Generated documentation is missing for this page.</p>')
</script>

<template>
  <div class="kiodoc-import" v-html="html" />
</template>
