/**
 * Stand-in for packages that a bundle names only in dead code.
 *
 * `@file-viewer/capability-mermaid` inlines cose-base and layout-base, but UMD
 * wrappers in two of its chunks still list them in an AMD `define([...])` that
 * follows `typeof exports == "object" && typeof module == "object"`, so it
 * never runs. Turbopack resolves that array anyway and fails the build, so
 * next.config.ts aliases the names here.
 */
export {};
