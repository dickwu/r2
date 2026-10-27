/** A JSON syntax error: the offset of the offending character, and what was expected there. */
export interface JsonSyntaxError {
  offset: number;
  reason: string;
}

/**
 * Finds the first syntax error in a JSON document, the same way on every engine.
 *
 * `JSON.parse` only says where it stopped on V8 ("… at position 12"); WebKit's
 * "JSON Parse error: …" carries no position, so the editor walks the grammar itself.
 * Returns null when the document is valid JSON.
 */
export function findJsonSyntaxError(text: string): JsonSyntaxError | null {
  let i = 0;

  class Stop extends Error {
    constructor(
      readonly at: number,
      readonly reason: string
    ) {
      super(reason);
    }
  }
  const fail = (reason: string, at = i): never => {
    throw new Stop(at, reason);
  };
  const describe = (at: number) =>
    at >= text.length ? 'the end of the document' : `"${text[at]}"`;

  const skipSpace = () => {
    while (i < text.length) {
      const c = text[i];
      if (c === ' ' || c === '\t' || c === '\n' || c === '\r') i++;
      else break;
    }
  };

  const literal = (word: string) => {
    for (let k = 0; k < word.length; k++) {
      if (text[i + k] !== word[k]) fail(`Unexpected ${describe(i + k)}`, i + k);
    }
    i += word.length;
  };

  const string = () => {
    i++; // opening quote
    while (i < text.length) {
      const c = text[i];
      if (c === '"') {
        i++;
        return;
      }
      if (c === '\\') {
        const e = text[i + 1];
        if (e === 'u') {
          for (let k = 2; k < 6; k++) {
            if (!/[0-9a-fA-F]/.test(text[i + k] ?? '')) fail('Invalid \\u escape', i + k);
          }
          i += 6;
        } else if (e !== undefined && '"\\/bfnrt'.includes(e)) {
          i += 2;
        } else {
          fail('Invalid escape in a string', i + 1);
        }
        continue;
      }
      if (c.charCodeAt(0) < 0x20) fail('Line breaks and control characters must be escaped');
      i++;
    }
    fail('Unterminated string');
  };

  const digits = () => {
    const start = i;
    while (i < text.length && text[i] >= '0' && text[i] <= '9') i++;
    if (i === start) fail(`Expected a digit, found ${describe(i)}`);
  };

  const number = () => {
    if (text[i] === '-') i++;
    if (text[i] === '0') {
      i++;
      if (text[i] >= '0' && text[i] <= '9') fail('Numbers cannot have leading zeros');
    } else {
      digits();
    }
    if (text[i] === '.') {
      i++;
      digits();
    }
    if (text[i] === 'e' || text[i] === 'E') {
      i++;
      if (text[i] === '+' || text[i] === '-') i++;
      digits();
    }
  };

  const value = (): void => {
    skipSpace();
    const c = text[i];
    if (c === '{') return object();
    if (c === '[') return array();
    if (c === '"') return string();
    if (c === 't') return literal('true');
    if (c === 'f') return literal('false');
    if (c === 'n') return literal('null');
    if (c === '-' || (c >= '0' && c <= '9')) return number();
    fail(`Expected a value, found ${describe(i)}`);
  };

  const object = () => {
    i++; // {
    skipSpace();
    if (text[i] === '}') {
      i++;
      return;
    }
    for (;;) {
      skipSpace();
      if (text[i] !== '"') {
        fail(
          text[i] === '}'
            ? 'Trailing comma before "}"'
            : `Expected a property name in double quotes, found ${describe(i)}`
        );
      }
      string();
      skipSpace();
      if (text[i] !== ':') fail(`Expected ":", found ${describe(i)}`);
      i++;
      value();
      skipSpace();
      if (text[i] === ',') {
        i++;
        continue;
      }
      if (text[i] === '}') {
        i++;
        return;
      }
      fail(`Expected "," or "}", found ${describe(i)}`);
    }
  };

  const array = () => {
    i++; // [
    skipSpace();
    if (text[i] === ']') {
      i++;
      return;
    }
    for (;;) {
      skipSpace();
      if (text[i] === ']') fail('Trailing comma before "]"');
      value();
      skipSpace();
      if (text[i] === ',') {
        i++;
        continue;
      }
      if (text[i] === ']') {
        i++;
        return;
      }
      fail(`Expected "," or "]", found ${describe(i)}`);
    }
  };

  try {
    value();
    skipSpace();
    if (i < text.length) fail(`Unexpected ${describe(i)} after the JSON value`);
    return null;
  } catch (error) {
    if (error instanceof Stop)
      return { offset: Math.min(error.at, text.length), reason: error.reason };
    throw error;
  }
}

/** The 1-based line and column of an offset, counting "\n" as the line break. */
export function lineAndColumn(text: string, offset: number): { line: number; column: number } {
  const before = text.slice(0, offset);
  const line = before.split('\n').length;
  const column = offset - before.lastIndexOf('\n');
  return { line, column };
}
