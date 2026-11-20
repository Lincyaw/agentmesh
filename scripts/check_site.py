import json
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


class SiteParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.references = []
        self.ids = []
        self.language = None
        self.videos = 0

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'html':
            self.language = attrs.get('lang')
        if 'id' in attrs:
            self.ids.append(attrs['id'])
        if tag == 'video':
            self.videos += 1
        for key in ['src', 'href', 'poster']:
            if attrs.get(key):
                self.references.append(attrs[key])


def main():
    root = Path(__file__).resolve().parents[1]
    site = root / 'site'
    parser = SiteParser()
    parser.feed((site / 'index.html').read_text())
    assert parser.language == 'en'
    assert len(parser.ids) == len(set(parser.ids)), 'duplicate HTML IDs'
    assert parser.videos == 1
    checked = 0
    for ref in parser.references:
        url = urlsplit(ref)
        if url.scheme or url.netloc:
            assert url.scheme == 'https', f'non-HTTPS external link: {ref}'
            continue
        if url.path:
            path = (site / unquote(url.path)).resolve()
            assert path.is_relative_to(site.resolve()), f'path escapes site: {ref}'
            assert path.is_file(), f'missing local asset: {ref}'
            checked += 1
        if url.fragment:
            assert url.fragment in parser.ids, f'missing anchor: {ref}'
    for asset in ['agentmesh-promo.mp4', 'promo-poster.png', 'favicon.svg']:
        assert (site / 'assets' / asset).stat().st_size > 0, f'empty asset: {asset}'
    print(json.dumps({'html_ids': len(parser.ids), 'local_references': checked, 'video': True, 'status': 'passed'}))


if __name__ == '__main__':
    main()
