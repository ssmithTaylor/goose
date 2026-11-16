import { useCallback, useEffect, useState } from 'react';
import { defineMessages, useIntl } from '../../../i18n';
import { useConfig } from '../../ConfigContext';
import { toastError } from '../../../toasts';
import { errorMessage } from '../../../utils/conversionUtils';
import type { View } from '../../../utils/navigationUtils';
import { Button } from '../../ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../../ui/card';
import type Model from './modelInterface';
import { SwitchModelModal } from './subcomponents/SwitchModelModal';

export const SUBAGENT_PROVIDER_KEY = 'GOOSE_SUBAGENT_PROVIDER';
export const SUBAGENT_MODEL_KEY = 'GOOSE_SUBAGENT_MODEL';

const i18n = defineMessages({
  title: {
    id: 'subagentModelSection.title',
    defaultMessage: 'Subagent default model',
  },
  description: {
    id: 'subagentModelSection.description',
    defaultMessage:
      'Subagents use a model the agent names when delegating first, then recipe settings, then this default, then the main chat model.',
  },
  sameAsMainChat: {
    id: 'subagentModelSection.sameAsMainChat',
    defaultMessage: 'Same as main chat',
  },
  chooseModel: {
    id: 'subagentModelSection.chooseModel',
    defaultMessage: 'Choose model',
  },
  modalTitle: {
    id: 'subagentModelSection.modalTitle',
    defaultMessage: 'Subagent default model',
  },
  modalDescription: {
    id: 'subagentModelSection.modalDescription',
    defaultMessage: 'Select the provider and model new subagents use by default.',
  },
  saveFailed: {
    id: 'subagentModelSection.saveFailed',
    defaultMessage: "Couldn't save the subagent default model",
  },
});

interface SubagentDefaultModel {
  provider: string | null;
  model: string | null;
}

function configString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value : null;
}

export default function SubagentDefaultModelCard({ setView }: { setView: (view: View) => void }) {
  const intl = useIntl();
  const { read, upsert, remove } = useConfig();
  const [current, setCurrent] = useState<SubagentDefaultModel | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let active = true;
    Promise.all([read(SUBAGENT_PROVIDER_KEY, false), read(SUBAGENT_MODEL_KEY, false)]).then(
      ([provider, model]) => {
        if (active) {
          setCurrent({ provider: configString(provider), model: configString(model) });
        }
      }
    );
    return () => {
      active = false;
    };
  }, [read]);

  const reportFailure = useCallback(
    (error: unknown) => {
      toastError({
        title: intl.formatMessage(i18n.saveFailed),
        msg: errorMessage(error),
        traceback: errorMessage(error),
      });
    },
    [intl]
  );

  const saveModel = async (model: Model) => {
    if (!model.provider || !model.name) {
      return false;
    }
    try {
      await upsert(SUBAGENT_PROVIDER_KEY, model.provider, false);
      await upsert(SUBAGENT_MODEL_KEY, model.name, false);
      setCurrent({ provider: model.provider, model: model.name });
      return true;
    } catch (error) {
      reportFailure(error);
      return false;
    }
  };

  const clearSubagentDefault = async () => {
    setSaving(true);
    try {
      await remove(SUBAGENT_PROVIDER_KEY, false);
      await remove(SUBAGENT_MODEL_KEY, false);
      setCurrent({ provider: null, model: null });
    } catch (error) {
      reportFailure(error);
    } finally {
      setSaving(false);
    }
  };

  const configured = current?.provider != null || current?.model != null;

  return (
    <Card className="pb-2 rounded-lg" data-testid="subagent-default-model">
      <CardHeader className="pb-0">
        <CardTitle>{intl.formatMessage(i18n.title)}</CardTitle>
        <CardDescription>{intl.formatMessage(i18n.description)}</CardDescription>
      </CardHeader>
      <CardContent className="px-2">
        {current === null ? (
          <div className="h-[36px]" />
        ) : configured ? (
          <div className="animate-in fade-in duration-100">
            <h3 className="text-text-primary">{current.model}</h3>
            <h4 className="text-xs text-text-secondary">{current.provider}</h4>
          </div>
        ) : (
          <h3 className="animate-in fade-in duration-100 text-text-primary">
            {intl.formatMessage(i18n.sameAsMainChat)}
          </h3>
        )}
        <div className="flex gap-2 pt-4">
          <Button size="sm" variant="default" onClick={() => setPickerOpen(true)}>
            {intl.formatMessage(i18n.chooseModel)}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => void clearSubagentDefault()}
            disabled={!configured || saving}
          >
            {intl.formatMessage(i18n.sameAsMainChat)}
          </Button>
        </div>
        {pickerOpen && (
          <SwitchModelModal
            sessionId={null}
            setView={setView}
            onClose={() => setPickerOpen(false)}
            titleOverride={intl.formatMessage(i18n.modalTitle)}
            descriptionOverride={intl.formatMessage(i18n.modalDescription)}
            sessionModel={current?.model}
            sessionProvider={current?.provider}
            saveModel={saveModel}
          />
        )}
      </CardContent>
    </Card>
  );
}
