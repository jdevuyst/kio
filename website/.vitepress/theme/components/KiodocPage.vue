<script setup>
import { computed } from 'vue'
import { kiodocHtml, kiodocSources } from '../../generated/kiodoc-html.mjs'
import SourceFooter from './SourceFooter.vue'

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
  <SourceFooter :source="kiodocSources[page]" />
</template>
