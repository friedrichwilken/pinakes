export default {
  title: 'Docs',
  description: 'A fixture VitePress site',
  themeConfig: {
    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started#install' },
          { text: 'Overview', link: '/guide/' },
          {
            text: 'Advanced',
            items: [
              { text: 'Deep \'Dive\'', link: '/guide/advanced/deep' },
            ],
          },
        ],
      },
      {
        text: 'Reference',
        items: [
          { text: 'API', link: '/reference/api.md' },
          { text: 'Missing Page', link: '/reference/missing' },
        ],
      },
    ],
  },
}
