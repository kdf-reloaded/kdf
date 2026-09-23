use super::IrnMetadata;

pub(crate) const IRN_RESPONSE_METADATA: IrnMetadata = IrnMetadata {
    tag: 1109,
    ttl: 60,
    prompt: false,
};

pub(crate) const IRN_REQUEST_METADATA: IrnMetadata = IrnMetadata {
    tag: 1108,
    ttl: 300,
    prompt: true,
};
