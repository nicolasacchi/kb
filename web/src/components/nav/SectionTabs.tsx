import { Link, useLocation } from "react-router-dom";
import { useExplicitKb } from "../../hooks/useActiveKb";
import { LISTS_TABS, WORKLOG_TABS, type SectionTab } from "../../lib/navItems";

// OL-L2 — a labelled sub-nav row for a door that owns more than one page
// (Worklog: Sessions · Slates, Lists: Lists · Notes). The active tab carries
// `aria-current="page"`; the row is a landmark named after its door.
export function SectionTabs({
  label,
  tabs,
}: {
  label: string;
  tabs: SectionTab[];
}) {
  const { pathname } = useLocation();
  const kb = useExplicitKb();
  return (
    <nav className="kb-sectiontabs" aria-label={label}>
      {tabs.map((tab) => {
        const on = tab.isActive(pathname);
        return (
          <Link
            key={tab.id}
            to={tab.href({ kb })}
            className={"kb-sectiontabs__tab" + (on ? " is-active" : "")}
            aria-current={on ? "page" : undefined}
            data-testid={tab.testid}
          >
            {tab.label}
          </Link>
        );
      })}
    </nav>
  );
}

export const WorklogTabs = () => <SectionTabs label="Worklog" tabs={WORKLOG_TABS} />;
export const ListsTabs = () => <SectionTabs label="Lists" tabs={LISTS_TABS} />;
