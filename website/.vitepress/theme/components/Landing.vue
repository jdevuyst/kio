<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, shallowRef } from 'vue'
import type { Component } from 'vue'
import { useData, withBase } from 'vitepress'
import { helloExampleFiles } from '../../generated/helloExample.mjs'
import { languageCount, supportedLanguages } from '../../generated/languages.mjs'
import { siteUrl } from '../../site.mjs'

const { isDark } = useData()

const replSection = ref<HTMLElement | null>(null)
const ReplComponent = shallowRef<Component | null>(null)
const replLoading = ref(false)
let replObserver: IntersectionObserver | undefined
let revealObserver: IntersectionObserver | undefined
const cueOpacity = ref(1)

function onHeroScroll() {
  cueOpacity.value = Math.max(0, Math.min(1, 1 - window.scrollY / (window.innerHeight * 0.35)))
}

async function launchRepl() {
  if (replLoading.value || ReplComponent.value) return
  replLoading.value = true
  try {
    const mod = await import('./ReplIsland.vue')
    ReplComponent.value = mod.default
  } catch {
    replLoading.value = false
  }
}

onMounted(() => {
  window.addEventListener('scroll', onHeroScroll, { passive: true })
  onHeroScroll()
  if (typeof IntersectionObserver === 'undefined') return
  replObserver = new IntersectionObserver((entries) => {
    if (entries.some((entry) => entry.isIntersecting)) {
      replObserver?.disconnect()
      launchRepl()
    }
  }, { rootMargin: '600px 0px' })
  if (replSection.value) replObserver.observe(replSection.value)

  const reduceMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
  const home = document.querySelector('.kio-home')
  if (home && !reduceMotion) {
    home.classList.add('js-reveal')
    revealObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add('revealed')
          revealObserver?.unobserve(entry.target)
        }
      }
    }, { threshold: 0.2 })
    home.querySelectorAll('[data-reveal]').forEach((el) => revealObserver?.observe(el))
  }
})

onBeforeUnmount(() => {
  window.removeEventListener('scroll', onHeroScroll)
  replObserver?.disconnect()
  revealObserver?.disconnect()
})

const moduleFile = helloExampleFiles.find((file) => file.name.endsWith('.pkg.kio')) ?? helloExampleFiles[0]
const sourceFile = helloExampleFiles.find((file) => !file.name.endsWith('.pkg.kio')) ?? helloExampleFiles[0]

const guarantees = [
  {
    title: 'Sound, decidable types',
    body: 'Type checking always terminates and catches type errors in Kio code before execution.'
  },
  {
    title: 'Strongly normalizing core',
    body: 'Kio elaborates to Kio\', a small core based on polymorphic lambda calculus with higher-kinded types and rank-N polymorphism, where every well-typed program reduces to a normal form.'
  },
  {
    title: 'Open-world compilation',
    body: 'Adding declarations to module bodies never breaks or changes the meaning of existing dependent code.'
  }
] as const

function toggleAppearance() {
  isDark.value = !isDark.value
}

function scrollToFeatures() {
  const target = document.getElementById('features')
  if (!target) return
  const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
  target.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' })
}

function scrollToRepl(event: MouseEvent) {
  const target = document.getElementById('repl')
  if (!target) return
  event.preventDefault()
  const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
  target.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'center' })
}

const installCommand = `curl -fsSL ${siteUrl}/install.sh | sh`
const copiedInstall = ref('')
let installCopyTimer: ReturnType<typeof setTimeout> | undefined

function markCopied(id: string) {
  copiedInstall.value = id
  clearTimeout(installCopyTimer)
  installCopyTimer = setTimeout(() => {
    copiedInstall.value = ''
  }, 1600)
}

// `navigator.clipboard` only exists in secure contexts (HTTPS / localhost), so
// fall back to a temporary textarea + execCommand when serving over plain HTTP.
function fallbackCopy(text: string): boolean {
  const area = document.createElement('textarea')
  area.value = text
  area.style.position = 'fixed'
  area.style.top = '-9999px'
  document.body.appendChild(area)
  area.focus()
  area.select()
  let ok = false
  try {
    ok = document.execCommand('copy')
  } catch {
    ok = false
  }
  document.body.removeChild(area)
  return ok
}

function copyInstall(id: string) {
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(installCommand).then(
      () => markCopied(id),
      () => {
        if (fallbackCopy(installCommand)) markCopied(id)
      }
    )
  } else if (fallbackCopy(installCommand)) {
    markCopied(id)
  }
}
</script>

<template>
  <main class="kio-home">
    <button
      class="home-appearance-toggle"
      type="button"
      :aria-label="isDark ? 'Switch to light mode' : 'Switch to dark mode'"
      @click="toggleAppearance"
    >
      <svg v-if="isDark" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41" />
      </svg>
      <svg v-else viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M21 14.7A8.5 8.5 0 0 1 9.3 3a7 7 0 1 0 11.7 11.7Z" />
      </svg>
    </button>

    <div class="home-first">
    <section class="home-hero">
      <div class="home-copy">
        <span class="home-logo-badge" data-reveal>
          <img class="home-logo" :src="withBase('/kio-logo-home.webp')" alt="Kio" width="600" height="467">
        </span>
        <p class="home-eyebrow" data-reveal>Ultra-portable · Embeddable · Statically typed</p>
        <h1 class="home-title" data-reveal>One package, {{ languageCount }} host languages.</h1>
        <p class="lead" data-reveal>
          Kio is a statically typed, embeddable programming language. Write a package once,
          then transpile it to the host language or load it dynamically into the host of your choice.
        </p>
        <div class="home-actions" data-reveal>
          <a class="primary-link" :href="withBase('/docs/')">Read the docs</a>
          <a class="secondary-link" href="#repl" @click="scrollToRepl">Try the REPL</a>
          <a class="secondary-link" href="https://github.com/jdevuyst/kio">
            <svg class="github-mark" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
              <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38v-1.33c-2.23.48-2.7-1.07-2.7-1.07-.36-.92-.89-1.17-.89-1.17-.73-.5.05-.49.05-.49.8.06 1.22.82 1.22.82.72 1.21 1.87.86 2.33.66.07-.52.28-.86.5-1.06-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82A7.7 7.7 0 0 1 8 3.9c.68 0 1.36.09 2 .27 1.53-1.03 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.28.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48v2.16c0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
            </svg>
            GitHub
          </a>
        </div>
        <div class="home-install" data-reveal>
          <code><span class="i-cmd">curl</span> <span class="i-flag">-fsSL</span> <span class="i-url">{{ siteUrl }}/install.sh</span> <span class="i-op">|</span> <span class="i-cmd">sh</span></code>
          <button
            type="button"
            class="home-install-copy"
            :class="{ copied: copiedInstall === 'hero' }"
            :aria-label="copiedInstall === 'hero' ? 'Copied to clipboard' : 'Copy install command'"
            @click="copyInstall('hero')"
          >
            <svg v-if="copiedInstall === 'hero'" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M5 13l4 4L19 7" /></svg>
            <svg v-else viewBox="0 0 24 24" aria-hidden="true" focusable="false"><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M6 15V6a2 2 0 0 1 2-2h9" /></svg>
          </button>
        </div>
      </div>
    </section>

    <section class="home-band" aria-label="Kio capabilities">
      <div class="capability" data-reveal>
        <h2>Portable</h2>
        <p>
          One package compiles to {{ languageCount }} host languages. Kio emits code in the
          host's own language — no separate runtime to ship.
        </p>
      </div>
      <div class="capability" data-reveal>
        <h2>Hosted</h2>
        <p>
          Kio packages are components, not standalone programs. The host supplies the
          package's capabilities and decides which exposed entries to call.
        </p>
      </div>
      <div class="capability" data-reveal>
        <h2>Typed</h2>
        <p>
          Kio is statically typed, including its declared host interfaces. Host
          implementations must honor those interfaces.
        </p>
      </div>
    </section>
      <button class="home-scroll-cue" :style="{ opacity: cueOpacity, pointerEvents: cueOpacity < 0.05 ? 'none' : 'auto' }" type="button" aria-label="Scroll to the features" @click="scrollToFeatures">
        <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M6 9l6 6 6-6" /></svg>
      </button>
    </div>

    <section id="features" class="home-feature" aria-labelledby="feature-portable">
      <div class="home-feature-copy" data-reveal>
        <p class="home-feature-eyebrow">Portability</p>
        <h2 id="feature-portable" class="home-feature-title">Write it once. Host it anywhere.</h2>
        <p>
          A package declares its build targets, and Kio emits idiomatic code for each one.
          The same source runs inside JavaScript, Python, Rust, and more — compiled ahead of
          time and linked in, or loaded dynamically.
        </p>
        <p>
          There is no ambient runtime and no hidden I/O. The host provides the numerics,
          strings, and effects your package uses, so what ships is just your logic.
        </p>
      </div>
      <figure class="home-feature-visual home-example-file" data-reveal>
        <figcaption>{{ moduleFile.name }}</figcaption>
        <pre class="home-code language-kio"><code v-html="moduleFile.highlightedHtml" /></pre>
      </figure>
    </section>

    <section class="home-feature reverse" aria-labelledby="feature-typed">
      <div class="home-feature-copy" data-reveal>
        <p class="home-feature-eyebrow">The interface</p>
        <h2 id="feature-typed" class="home-feature-title">A typed contract with the host.</h2>
        <p>
          A package names the host types and functions it needs, and the typed entries it
          exposes back. The host reads that contract, supplies the implementations, and calls
          the entries it wants.
        </p>
        <p>
          Because the contract is statically typed, Kio checks your package against it at
          compile time.
        </p>
        <p>
          The contract is versioned, too: <code>kio sig</code> records it and classifies every
          change as compatible or breaking before it ships.
        </p>
      </div>
      <figure class="home-feature-visual home-example-file" data-reveal>
        <figcaption>{{ sourceFile.name }}</figcaption>
        <pre class="home-code language-kio"><code v-html="sourceFile.highlightedHtml" /></pre>
      </figure>
    </section>

    <section class="home-lang-section" aria-labelledby="feature-lang">
      <div class="home-section-head">
        <p class="home-feature-eyebrow">The language</p>
        <h2 id="feature-lang" class="home-feature-title">A small surface that composes.</h2>
        <p class="home-section-lead">
          A small core. Even control structures are library-defined.
        </p>
      </div>
      <div class="home-lang-grid">
        <div class="lang-feature">
          <h3>Library-defined control</h3>
          <code>if! ok { yes() } else { no() }</code>
          <p>
            Import elaborators like any other name. They generate typed code at compile time:
            <code>reorder_prod!((a, b), B &amp; A)</code> reorders a product to match the requested
            type, and familiar control forms such as <code>if!</code> and <code>match!</code>
            are library-defined too.
          </p>
        </div>
        <div class="lang-feature">
          <h3>Structural products &amp; sums</h3>
          <code>A &amp; B · A | B</code>
          <p>
            Compose types directly with <code>&amp;</code> and <code>|</code>, and
            pattern-match on them. Row-typed records and variants emerge from the same primitives.
          </p>
        </div>
        <div class="lang-feature">
          <h3>UFCS</h3>
          <code>r.&gt;f(x)</code>
          <p>
            Call any function receiver-first: <code>r.&gt;f(x)</code> is exactly
            <code>f(r, x)</code>. Four variants: <code>.&gt; .&gt;&gt; .&lt; .&lt;&lt;</code>.
          </p>
        </div>
        <div class="lang-feature">
          <h3>User-defined operators</h3>
          <code>op _ ? _ : __ { … }</code>
          <p>
            Every operator is yours to define — from prefix, infix, postfix, and n-ary to
            bracketed variadic forms for lists and dicts.
          </p>
        </div>
        <div class="lang-feature">
          <h3>Provable equivalence</h3>
          <code>equiv { … }</code>
          <p>
            Assert that two expressions reduce to the same normal form — the compiler proves it,
            decidably and exhaustively, running nothing.
          </p>
        </div>
        <div class="lang-feature">
          <h3>Library-defined sequencing</h3>
          <code>do! bind { … }</code>
          <p>
            Each <code>let x &lt;- action;</code> step sequences actions with the bind function
            you supply. <code>do!</code> is imported library code.
          </p>
        </div>
      </div>
    </section>

    <section class="home-guarantees-section" aria-labelledby="feature-core">
      <div class="home-section-head">
        <p class="home-feature-eyebrow">The core — guarantees, not vibes</p>
        <h2 id="feature-core" class="home-feature-title">A small core with strong guarantees.</h2>
        <p class="home-section-lead">
          Kio elaborates down to a compact, well-understood core. That is where the
          language's formal promises live.
        </p>
      </div>
      <div class="home-guarantees">
        <div v-for="item in guarantees" :key="item.title" class="guarantee">
          <h3>{{ item.title }}</h3>
          <p>{{ item.body }}</p>
        </div>
      </div>
    </section>

    <section class="home-feature" aria-labelledby="feature-dx">
      <div class="home-feature-copy">
        <p class="home-feature-eyebrow">The tooling — fast feedback</p>
        <h2 id="feature-dx" class="home-feature-title">Tooling that keeps up with you.</h2>
        <p>
          Kio is built for a tight loop: a fast typechecker with clear diagnostics, quick
          builds, and editor support that catches mistakes before you run anything.
        </p>
      </div>
      <ul class="home-feature-visual home-dx-list">
        <li>
          <strong>Fast builds</strong>
          <span>Parallel compilation, cached across runs.</span>
        </li>
        <li>
          <strong>Clear diagnostics</strong>
          <span>Precise errors with source context, right in your editor.</span>
        </li>
        <li>
          <strong>Editor tooling</strong>
          <span>An LSP, a tree-sitter grammar, and a VS Code extension.</span>
        </li>
      </ul>
    </section>

    <section id="repl" class="home-try" aria-labelledby="feature-try">
      <div class="home-section-head">
        <p class="home-feature-eyebrow">The REPL — live</p>
        <h2 id="feature-try" class="home-feature-title">Try Kio in your browser.</h2>
        <p class="home-section-lead">
          Browse packages, inspect types, normalize expressions — it all runs in your browser, no server.
        </p>
      </div>
      <div ref="replSection" class="home-try-repl">
        <component :is="ReplComponent" v-if="ReplComponent" initial-input=":help" />
        <button
          v-else
          type="button"
          class="home-repl-launch"
          :class="{ loading: replLoading }"
          :disabled="replLoading"
          @click="launchRepl"
        >
          <span v-if="replLoading" class="home-repl-launch-spinner" aria-hidden="true" />
          <svg v-else class="home-repl-launch-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M8 5v14l11-7z" />
          </svg>
          <span>{{ replLoading ? 'Starting the REPL…' : 'Launch the REPL' }}</span>
        </button>
      </div>
    </section>

    <section class="home-targets-section" aria-labelledby="feature-targets">
      <div class="home-section-head">
        <p class="home-feature-eyebrow">Host languages</p>
        <h2 id="feature-targets" class="home-feature-title">Runs in {{ languageCount }} host languages.</h2>
        <p class="home-section-lead">
          Pick a host to see how Kio packages plug in.
        </p>
      </div>
      <div class="home-target-grid" aria-label="Supported host languages">
        <a
          v-for="language in supportedLanguages"
          :key="language.id"
          class="home-target-link"
          :href="withBase(language.docRoute)"
        >
          {{ language.name }}
        </a>
      </div>
    </section>

    <section class="home-cta" aria-label="Get started with Kio">
      <h2>Ready to dig in?</h2>
      <p>Install it in one line, or read up on it first.</p>
      <div class="home-install">
        <code><span class="i-cmd">curl</span> <span class="i-flag">-fsSL</span> <span class="i-url">{{ siteUrl }}/install.sh</span> <span class="i-op">|</span> <span class="i-cmd">sh</span></code>
        <button
          type="button"
          class="home-install-copy"
          :class="{ copied: copiedInstall === 'cta' }"
          :aria-label="copiedInstall === 'cta' ? 'Copied to clipboard' : 'Copy install command'"
          @click="copyInstall('cta')"
        >
          <svg v-if="copiedInstall === 'cta'" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M5 13l4 4L19 7" /></svg>
          <svg v-else viewBox="0 0 24 24" aria-hidden="true" focusable="false"><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M6 15V6a2 2 0 0 1 2-2h9" /></svg>
        </button>
      </div>
      <div class="home-actions">
        <a class="primary-link" :href="withBase('/docs/')">Read the docs</a>
        <a class="secondary-link" href="https://github.com/jdevuyst/kio">
          <svg class="github-mark" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
            <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38v-1.33c-2.23.48-2.7-1.07-2.7-1.07-.36-.92-.89-1.17-.89-1.17-.73-.5.05-.49.05-.49.8.06 1.22.82 1.22.82.72 1.21 1.87.86 2.33.66.07-.52.28-.86.5-1.06-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82A7.7 7.7 0 0 1 8 3.9c.68 0 1.36.09 2 .27 1.53-1.03 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.28.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48v2.16c0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
          </svg>
          GitHub
        </a>
      </div>
    </section>

    <footer class="home-footer">
      <p class="home-footer-blog"><a :href="withBase('/blog/')">Read the blog →</a></p>
      <p>Built almost entirely with AI, under maintainer direction and review.</p>
      <p>Dual-licensed under MIT or Apache-2.0.</p>
    </footer>
  </main>
</template>
