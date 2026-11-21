//! A pinned relay address retains hostname-based TLS verification without external DNS.
use futures::future::BoxFuture;
use iroh::dns::{BoxIter, DnsError, DnsResolver, Resolver, TxtRecordData};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Debug)]
struct PinnedRelay {
    hostname: String,
    address: IpAddr,
}

impl PinnedRelay {
    fn matches(&self, host: &str) -> bool {
        host.trim_end_matches('.')
            .eq_ignore_ascii_case(&self.hostname)
    }
}

impl Resolver for PinnedRelay {
    fn lookup_ipv4(&self, host: String) -> BoxFuture<'static, Result<BoxIter<Ipv4Addr>, DnsError>> {
        let value = match self.address {
            IpAddr::V4(ip) if self.matches(&host) => Some(ip),
            _ => None,
        };
        Box::pin(async move { Ok(Box::new(value.into_iter()) as BoxIter<_>) })
    }
    fn lookup_ipv6(&self, host: String) -> BoxFuture<'static, Result<BoxIter<Ipv6Addr>, DnsError>> {
        let value = match self.address {
            IpAddr::V6(ip) if self.matches(&host) => Some(ip),
            _ => None,
        };
        Box::pin(async move { Ok(Box::new(value.into_iter()) as BoxIter<_>) })
    }
    fn lookup_txt(
        &self,
        _host: String,
    ) -> BoxFuture<'static, Result<BoxIter<TxtRecordData>, DnsError>> {
        Box::pin(async { Ok(Box::new(std::iter::empty()) as BoxIter<_>) })
    }
    fn clear_cache(&self) {}
    fn reset(&self) -> Box<dyn Resolver> {
        Box::new(self.clone())
    }
}

pub fn pinned(url: &iroh::RelayUrl, address: IpAddr) -> anyhow::Result<DnsResolver> {
    let hostname = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("relay URL has no hostname"))?
        .to_owned();
    anyhow::ensure!(
        !address.is_unspecified() && !address.is_multicast(),
        "invalid pinned relay IP"
    );
    Ok(DnsResolver::custom(PinnedRelay { hostname, address }))
}
