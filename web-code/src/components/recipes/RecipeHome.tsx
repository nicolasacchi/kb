import { Link } from "react-router-dom";
import type { KbcCatalogEntry } from "../../api/types";
import { RECIPE_INTENT_ORDER, recipeIntentLabel } from "../../lib/recipeAddr";
import { recipeRunUrl } from "../../lib/recipeUrl";
import RecipeTrustBadge from "./RecipeTrustBadge";
import { Icon } from "../icons";
import EmptyState from "../EmptyState";

export interface RecipeHomeProps {
  repo: string;
  recipes: KbcCatalogEntry[];
}

const HOME_LABEL: Record<string, string> = {
  builtin: "builtin",
  repo: "repo",
  server: "server",
};

/// D11's recipe home: intent GROUPS as sections, each listing its recipes
/// with home + trust chips. Choosing one navigates into the auto-form —
/// pre-filling the scope chip and composing the CLI line are that page's
/// job (`Recipes.tsx`), not this list's.
export default function RecipeHome({ repo, recipes }: RecipeHomeProps) {
  if (recipes.length === 0) {
    return (
      <EmptyState
        icon={<Icon.List />}
        title="No recipes"
        hint="This repo's catalog is empty — no builtins, server rows, or `.kbc/recipes/*.toml` files."
      />
    );
  }

  const byIntent = new Map<string, KbcCatalogEntry[]>();
  for (const r of recipes) {
    const list = byIntent.get(r.intent) ?? [];
    list.push(r);
    byIntent.set(r.intent, list);
  }
  // Known groups first, in the closed taxonomy's own order; an unrecognized
  // future intent still gets its own section rather than being dropped.
  const order = [
    ...RECIPE_INTENT_ORDER.filter((i) => byIntent.has(i)),
    ...[...byIntent.keys()].filter((i) => !(RECIPE_INTENT_ORDER as readonly string[]).includes(i)),
  ];

  return (
    <div className="kbc-recipe-home" data-kbc-recipe-home>
      {order.map((intent) => {
        const group = byIntent.get(intent) ?? [];
        return (
          <section key={intent} className="kbc-recipe-home__group" data-kbc-recipe-intent={intent}>
            <h2 className="kbc-recipe-home__group-title">{recipeIntentLabel(intent)}</h2>
            <ul className="kbc-recipe-home__list">
              {group.map((r) => (
                <li key={r.slug} className="kbc-recipe-home__item">
                  <Link
                    to={recipeRunUrl(repo, { slug: r.slug, params: {}, ctx: {} })}
                    className="kbc-recipe-home__card"
                    data-kbc-recipe-home-card={r.slug}
                  >
                    <span className="kbc-recipe-home__card-title">{r.title}</span>
                    <span className="kbc-recipe-home__card-chips">
                      <span
                        className={`kbc-recipe-home__home-chip kbc-recipe-home__home-chip--${r.home}`}
                        data-kbc-recipe-home-chip={r.home}
                      >
                        {HOME_LABEL[r.home] ?? r.home}
                      </span>
                      <RecipeTrustBadge state={r.trust} />
                    </span>
                    {r.description_md && (
                      <span className="kbc-recipe-home__card-desc">{r.description_md}</span>
                    )}
                    {r.shadowed_by && (
                      <span className="kbc-recipe-home__card-shadow" data-kbc-recipe-shadowed-by={r.shadowed_by}>
                        shadows a {HOME_LABEL[r.shadowed_by] ?? r.shadowed_by} recipe of the same slug
                      </span>
                    )}
                  </Link>
                </li>
              ))}
            </ul>
          </section>
        );
      })}
    </div>
  );
}
