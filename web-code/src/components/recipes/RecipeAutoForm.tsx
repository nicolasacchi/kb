import type { KbcParamSpec } from "../../api/types";
import type { ParamFieldError } from "../../lib/recipeUrl";
import { recipeParamDefaultString } from "../../lib/recipeUrl";

export interface RecipeAutoFormProps {
  specs: readonly KbcParamSpec[];
  params: Record<string, string>;
  onParamChange: (name: string, value: string) => void;
  /// Client-side mirror errors (`recipeValidateAll`) — caught before a
  /// round trip.
  errors: readonly ParamFieldError[];
  /// The server's own 400, when one happened — rendered on the SAME field
  /// rather than a page-level banner, per this unit's brief ("server 400s
  /// are rendered with the field").
  serverFieldErrors?: Readonly<Record<string, string>>;
  scope: string;
  onScopeChange: (value: string) => void;
  ctx: Readonly<Record<string, string>>;
  onCtxChange: (field: "path" | "symbol" | "ref", value: string) => void;
  /// Best-effort values read from the reader's own nav history — never
  /// silently applied without saying so (the chip IS the "saying so").
  ctxAutoDetected: Readonly<Partial<Record<"path" | "symbol" | "ref", string>>>;
}

function fieldErrorFor(name: string, errors: readonly ParamFieldError[], serverFieldErrors?: Readonly<Record<string, string>>): string | undefined {
  return serverFieldErrors?.[name] ?? errors.find((e) => e.name === name)?.message;
}

function ParamField({
  spec,
  value,
  onChange,
  error,
}: {
  spec: KbcParamSpec;
  value: string;
  onChange: (v: string) => void;
  error?: string;
}) {
  const inputId = `kbc-recipe-p-${spec.name}`;
  return (
    <div className="kbc-recipe-form__field" data-kbc-recipe-field={spec.name}>
      <label htmlFor={inputId} className="kbc-recipe-form__label">
        {spec.name}
        {spec.required && <span className="kbc-recipe-form__required" title="required">*</span>}
      </label>
      {spec.type === "bool" ? (
        <input
          id={inputId}
          type="checkbox"
          checked={value === "true"}
          onChange={(e) => onChange(e.target.checked ? "true" : "false")}
          data-kbc-recipe-input="bool"
        />
      ) : spec.type === "enum" ? (
        <select
          id={inputId}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          data-kbc-recipe-input="enum"
        >
          <option value="">{spec.required ? "— choose —" : "(recipe default)"}</option>
          {(spec.values ?? []).map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
      ) : spec.type === "int" || spec.type === "float" ? (
        <input
          id={inputId}
          type="number"
          value={value}
          min={spec.min}
          max={spec.max}
          step={spec.type === "int" ? 1 : "any"}
          onChange={(e) => onChange(e.target.value)}
          data-kbc-recipe-input={spec.type}
        />
      ) : (
        <input
          id={inputId}
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          data-kbc-recipe-input={spec.type}
        />
      )}
      {spec.description && <p className="kbc-recipe-form__hint">{spec.description}</p>}
      {(spec.min !== undefined || spec.max !== undefined) && (
        <p className="kbc-recipe-form__range">
          {spec.min !== undefined && spec.max !== undefined
            ? `${spec.min}–${spec.max}`
            : spec.min !== undefined
              ? `≥ ${spec.min}`
              : `≤ ${spec.max}`}
        </p>
      )}
      {error && (
        <p className="kbc-recipe-form__error" role="alert" data-kbc-recipe-field-error>
          {error}
        </p>
      )}
    </div>
  );
}

/// The typed param → form generator (D11): one field per `KbcParamSpec`,
/// client-validated to the SAME ranges the server enforces
/// (`lib/recipeUrl.ts`'s `recipeParamValidate`), plus the scope override and
/// the `$context` overrides section. Every value here is a raw STRING —
/// `recipeParamToQueryValue`/`recipeParamValidate` do the typed
/// interpretation, this component only collects text.
export default function RecipeAutoForm({
  specs,
  params,
  onParamChange,
  errors,
  serverFieldErrors,
  scope,
  onScopeChange,
  ctx,
  onCtxChange,
  ctxAutoDetected,
}: RecipeAutoFormProps) {
  const hasAutoDetected = Object.values(ctxAutoDetected).some((v) => v !== undefined);
  return (
    <form className="kbc-recipe-form" data-kbc-recipe-form onSubmit={(e) => e.preventDefault()}>
      <div className="kbc-recipe-form__field" data-kbc-recipe-field="scope">
        <label htmlFor="kbc-recipe-scope" className="kbc-recipe-form__label">
          Scope
        </label>
        <input
          id="kbc-recipe-scope"
          type="text"
          value={scope}
          onChange={(e) => onScopeChange(e.target.value)}
          placeholder="(recipe default — $context.scope)"
          data-kbc-recipe-input="scope"
        />
        <p className="kbc-recipe-form__hint">
          A kbc-scope/1 expression overriding the recipe's own. Empty = the recipe's default applies.
        </p>
      </div>

      {specs.map((spec) => (
        <ParamField
          key={spec.name}
          spec={spec}
          value={params[spec.name] ?? recipeParamDefaultString(spec)}
          onChange={(v) => onParamChange(spec.name, v)}
          error={fieldErrorFor(spec.name, errors, serverFieldErrors)}
        />
      ))}

      <details className="kbc-recipe-form__context" data-kbc-recipe-context>
        <summary>Context overrides ($context.path / .symbol / .ref)</summary>
        {hasAutoDetected && (
          <p className="kbc-recipe-form__context-chip" data-kbc-recipe-context-chip>
            Using context from your last open file — override any field below.
          </p>
        )}
        {(["path", "symbol", "ref"] as const).map((field) => (
          <div className="kbc-recipe-form__field" key={field} data-kbc-recipe-field={`ctx.${field}`}>
            <label htmlFor={`kbc-recipe-ctx-${field}`} className="kbc-recipe-form__label">
              {field}
            </label>
            <input
              id={`kbc-recipe-ctx-${field}`}
              type="text"
              value={ctx[field] ?? ""}
              placeholder={ctxAutoDetected[field] ?? ""}
              onChange={(e) => onCtxChange(field, e.target.value)}
              data-kbc-recipe-input={`ctx.${field}`}
            />
          </div>
        ))}
      </details>
    </form>
  );
}
