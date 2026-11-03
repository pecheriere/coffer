// Checks a response against docs/api/openapi.json: its status must be documented for
// that route, and its body must match what is documented for that status.
import { readFileSync } from 'node:fs';

import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';

const DOCUMENT_ID = 'https://coffer.invalid/openapi.json';

export const openapi = JSON.parse(
  readFileSync(new URL('../../../docs/api/openapi.json', import.meta.url), 'utf8'),
);

// The document says what a response holds, and nothing else may be in it. Each object schema
// with `properties` is closed where it stands: its branches, being in `oneOf`, each list
// their own. `allOf` would need more, and the document has none, which is checked here.
function closed(schema) {
  if (Array.isArray(schema)) return schema.map(closed);
  if (schema === null || typeof schema !== 'object') return schema;
  if (schema.allOf) throw new Error('a schema with allOf needs unevaluatedProperties here');
  const copy = Object.fromEntries(Object.entries(schema).map(([key, value]) => [key, closed(value)]));
  if (copy.properties && typeof schema.properties.type !== 'string' && copy.additionalProperties === undefined) {
    copy.additionalProperties = false;
  }
  return copy;
}

const ajv = new Ajv2020({
  strict: false,
  allErrors: true,
  validateSchema: false,
  formats: { int32: true, int64: true, password: true },
});
addFormats(ajv);
ajv.addSchema({ ...closed(openapi), $id: DOCUMENT_ID });

const escaped = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// A literal path before any template that could also match it, as a router would.
const routes = Object.keys(openapi.paths)
  .sort((a, b) => Number(a.includes('{')) - Number(b.includes('{')))
  .map((template) => ({
    template,
    pattern: new RegExp(
      `^${template.split(/\{[^}]+\}/).map(escaped).join('[^/]+')}$`,
    ),
  }));

const pointer = (...parts) =>
  parts.map((part) => part.replaceAll('~', '~0').replaceAll('/', '~1')).join('/');

const validators = new Map();

function validatorFor(template, method, status, type) {
  const key = `${method} ${template} ${status} ${type}`;
  if (!validators.has(key)) {
    const path = pointer('paths', template, method, 'responses', String(status), 'content', type);
    validators.set(key, ajv.compile({ $ref: `${DOCUMENT_ID}#/${path}/schema` }));
  }
  return validators.get(key);
}

// The problems with a response, none if it matches the document.
export function problemsWith(method, path, status, type, body) {
  const route = routes.find(({ pattern }) => pattern.test(path));
  if (!route) return [`${path} is not documented`];
  const operation = openapi.paths[route.template][method.toLowerCase()];
  if (!operation) return [`${method} ${route.template} is not documented`];
  const response = operation.responses[String(status)];
  if (!response) return [`${method} ${route.template} does not document ${status}`];
  if (!response.content) {
    return body === undefined ? [] : [`${method} ${route.template} ${status} documents no body`];
  }
  if (!response.content[type]) {
    return [`${method} ${route.template} ${status} does not document ${type}`];
  }
  const validate = validatorFor(route.template, method.toLowerCase(), status, type);
  if (validate(body)) return [];
  return validate.errors.map(({ instancePath, message }) => `${instancePath} ${message}`);
}
