export interface CatalogEntry {
  name: string;
  url: string;
  icon: string;
}

export const MESSENGER_CATALOG: CatalogEntry[] = [
  { name: 'Instagram',    url: 'https://www.instagram.com/',            icon: 'instagram' },
  { name: 'Facebook',     url: 'https://www.facebook.com/',             icon: 'facebook'  },
  { name: 'Discord',      url: 'https://discord.com/app',               icon: 'discord'   },
  { name: 'Slack',        url: 'https://app.slack.com/',                icon: 'slack'     },
  { name: 'Signal',       url: 'https://app.signal.org/',               icon: 'signal'    },
  { name: 'LinkedIn',     url: 'https://www.linkedin.com/messaging/',   icon: 'linkedin'  },
  { name: 'X (Twitter)',  url: 'https://x.com/messages',                icon: 'twitter'   },
];
