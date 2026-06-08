declare module 'bun:test' {
  export function describe(name: string, fn: () => void): void;
  export function test(name: string, fn: () => void | Promise<void>): void;
  export function beforeEach(fn: () => void | Promise<void>): void;
  export function afterEach(fn: () => void | Promise<void>): void;
  export function beforeAll(fn: () => void | Promise<void>): void;
  export function afterAll(fn: () => void | Promise<void>): void;

  interface Matchers<T> {
    toBe(expected: T): void;
    toEqual(expected: unknown): void;
    toContain(expected: unknown): void;
    toBeNull(): void;
    toBeUndefined(): void;
    toBeTruthy(): void;
    toBeFalsy(): void;
  }

  export const expect: {
    <T>(actual: T): Matchers<T> & { not: Matchers<T> };
  };

  export const mock: {
    (fn?: (...args: unknown[]) => unknown): (...args: unknown[]) => unknown;
    module(specifier: string, factory: () => unknown): void;
  };
}
