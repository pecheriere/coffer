// The strict reading of docs/api/openapi.json: a body holding a field the document does not
// list is refused, including where the document builds a shape with `allOf`.
import { expect, test } from '@playwright/test';

import { closed, problemsWithSchema } from '../support/openapi.mjs';

const money = { currency: 'EUR', minor_units: 1250 };
const mismatch = {
  status: 'mismatch',
  outflow: { currency: 'USD', minor_units: -125_000 },
  inflow: money,
  expected_inflow: money,
  rate_on: '2026-10-28',
  manual_rate: false,
};

test('an allOf of objects is one object: every member field is allowed and nothing more', () => {
  const schema = closed({
    allOf: [
      { type: 'object', required: ['a'], properties: { a: { type: 'string' } } },
      { type: 'object', required: ['b'], properties: { b: { type: 'string' } } },
    ],
  });
  expect(schema.additionalProperties).toBe(false);
  expect(Object.keys(schema.properties)).toEqual(['a', 'b']);
  expect(schema.required).toEqual(['a', 'b']);
});

test('a member of an allOf that is not a plain object is refused, not guessed at', () => {
  expect(() =>
    closed({ allOf: [{ type: 'object', properties: { a: {} }, additionalProperties: true }] }),
  ).toThrow('an allOf member must be an object');
});

test('the mismatch shape of a rate check takes its own fields and the tag, and no other', () => {
  expect(problemsWithSchema('RateCheck', mismatch)).toEqual([]);
  expect(problemsWithSchema('RateCheck', { ...mismatch, surprise: 1 })).not.toEqual([]);
  expect(problemsWithSchema('RateCheck', { status: 'mismatch' })).not.toEqual([]);
});
