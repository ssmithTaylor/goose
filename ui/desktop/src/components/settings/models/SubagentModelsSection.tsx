import { defineMessages, useIntl } from '../../../i18n';
import type { View } from '../../../utils/navigationUtils';
import SubagentDefaultModelCard from './SubagentDefaultModelCard';
import SubagentRoutingCard from './SubagentRoutingCard';

const i18n = defineMessages({
  title: {
    id: 'subagentModelsSection.title',
    defaultMessage: 'Subagent models',
  },
});

export default function SubagentModelsSection({ setView }: { setView: (view: View) => void }) {
  const intl = useIntl();

  return (
    <section
      id="subagent-models"
      className="space-y-4 pr-4 pt-6"
      aria-labelledby="subagent-models-title"
    >
      <h2 id="subagent-models-title" className="text-lg font-medium text-text-primary">
        {intl.formatMessage(i18n.title)}
      </h2>
      <SubagentDefaultModelCard setView={setView} />
      <SubagentRoutingCard setView={setView} />
    </section>
  );
}
